use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    Chat,
    CodeEdit,
    ShellExec,
    WebSearch,
    Vision,
    LongContext,
    Approval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub capabilities: Vec<AgentCapability>,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
    pub project_root: Option<PathBuf>,
    pub history: Vec<ChatTurn>,
    /// Per-call model override. When `None`, the adapter falls back to its
    /// configured `model_hint`. Used by the Aider-style architect/editor split
    /// in `chat.rs` to run two phases against two different upstream models.
    #[serde(default)]
    pub model: Option<String>,
    /// Per-call reasoning-effort hint (`minimal | low | medium | high`, Codex
    /// CLI parity). Already normalized + resolved (per-prompt override over the
    /// global config default) by `orchestrator::reasoning::resolve` before it
    /// reaches here, so it's either a canonical level or `None`. Adapters that
    /// can forward it (the gateway → reasoning upstreams) do; the rest ignore it.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Started { agent_id: String, run_id: Option<String> },
    Token { delta: String },
    Reasoning { text: String },
    ToolCall { name: String, args: serde_json::Value, preview: Option<String> },
    ToolResult { name: String, ok: bool, summary: String, duration_ms: Option<i64> },
    FileEdit { path: PathBuf, lines_changed: i64 },
    ApprovalRequest {
        run_id: String,
        tool: Option<String>,
        preview: Option<String>,
        choices: Vec<String>,
        request: serde_json::Value,
    },
    ApprovalResolved { run_id: String, choice: String },
    Error { message: String },
    Done { total_tokens: Option<u64>, run_id: Option<String> },
}

impl AgentEvent {
    /// Returns a redacted clone for anything that reaches a display surface
    /// (an approval prompt, the mobile UI) or gets persisted (the trace
    /// store). `ToolCall` and `ApprovalRequest` are the only variants that
    /// carry free-form, model/tool-supplied `preview`/`args`/`request`
    /// content — and a shell command or file write can legitimately embed a
    /// live credential (e.g. `curl -H "Authorization: Bearer sk-..."`) that
    /// would otherwise display and persist in plaintext. Uses
    /// `redact::redact_json_value`/`redact_text`, NOT
    /// `observability::sentry::redact` — that one is built for telemetry
    /// export and nukes entire fields wholesale, which would blank out the
    /// actual content the user needs to see to approve or deny intelligently.
    /// Every other variant passes through unchanged (nothing else carries
    /// free-form tool-supplied text of this shape).
    pub fn redacted_for_display(&self) -> AgentEvent {
        match self {
            AgentEvent::ToolCall { name, args, preview } => {
                let mut args = args.clone();
                crate::redact::redact_json_value(&mut args);
                AgentEvent::ToolCall {
                    name: name.clone(),
                    args,
                    preview: preview.as_deref().map(crate::redact::redact_text),
                }
            }
            AgentEvent::ApprovalRequest { run_id, tool, preview, choices, request } => {
                let mut request = request.clone();
                crate::redact::redact_json_value(&mut request);
                AgentEvent::ApprovalRequest {
                    run_id: run_id.clone(),
                    tool: tool.clone(),
                    preview: preview.as_deref().map(crate::redact::redact_text),
                    choices: choices.clone(),
                    request,
                }
            }
            other => other.clone(),
        }
    }
}

#[async_trait::async_trait]
pub trait AgentAdapter: Send + Sync {
    fn descriptor(&self) -> AgentDescriptor;
    async fn health_check(&self) -> bool;
    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_for_display_masks_tool_call_preview_and_args() {
        let evt = AgentEvent::ToolCall {
            name: "run_shell".into(),
            args: serde_json::json!({
                "cmd": "curl -H \"Authorization: Bearer sk-abcdefghijklmnopqrstuvwxyz0123\" https://x",
            }),
            preview: Some("curl -H \"Authorization: Bearer sk-abcdefghijklmnopqrstuvwxyz0123\" https://x".into()),
        };
        let redacted = evt.redacted_for_display();
        let AgentEvent::ToolCall { name, args, preview } = &redacted else {
            panic!("expected ToolCall");
        };
        assert_eq!(name, "run_shell");
        assert!(!args.to_string().contains("sk-abcdefghijklmnopqrstuvwxyz0123"));
        assert!(!preview.as_ref().unwrap().contains("sk-abcdefghijklmnopqrstuvwxyz0123"));
        assert!(preview.as_ref().unwrap().contains("[REDACTED]"));
    }

    #[test]
    fn redacted_for_display_masks_approval_request_request_field() {
        let evt = AgentEvent::ApprovalRequest {
            run_id: "r1".into(),
            tool: Some("write_file".into()),
            preview: Some("writing api_key = supersecretvalue1234 to .env".into()),
            choices: vec!["approve".into(), "deny".into()],
            request: serde_json::json!({ "path": ".env", "contents": "api_key = supersecretvalue1234" }),
        };
        let redacted = evt.redacted_for_display();
        let AgentEvent::ApprovalRequest { run_id, tool, preview, choices, request } = &redacted
        else {
            panic!("expected ApprovalRequest");
        };
        assert_eq!(run_id, "r1");
        assert_eq!(tool.as_deref(), Some("write_file"));
        assert_eq!(choices, &vec!["approve".to_string(), "deny".to_string()]);
        assert!(!preview.as_ref().unwrap().contains("supersecretvalue1234"));
        assert!(!request.to_string().contains("supersecretvalue1234"));
        // Structural, non-secret fields (the target path) survive.
        assert_eq!(request["path"], ".env");
    }

    /// Every other variant must pass through byte-for-byte — this method
    /// exists specifically for the two variants that carry free-form
    /// tool-supplied text; anything else redacting would be a silent
    /// behavior change with no security benefit (e.g. `Token`/`Reasoning`
    /// carry the model's own streamed prose, not tool-call payloads).
    #[test]
    fn redacted_for_display_passes_through_other_variants_unchanged() {
        let evt = AgentEvent::Token { delta: "hello sk-abcdefghijklmnopqrstuvwxyz0123".into() };
        let redacted = evt.redacted_for_display();
        let AgentEvent::Token { delta } = &redacted else {
            panic!("expected Token");
        };
        assert_eq!(delta, "hello sk-abcdefghijklmnopqrstuvwxyz0123");
    }
}
