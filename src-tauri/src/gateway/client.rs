//! Cortex Gateway client — wraps the OpenAI-compatible /v1/chat/completions
//! AND the richer /v1/runs lifecycle (tool events, approval gates, SSE).

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct GatewayClient {
    pub base_url: String,
    pub api_key: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct ModelList {
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ModelInfo {
    pub id: String,
}

// ---------- /v1/chat/completions (kept for fallback / parity) ----------

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionChunk {
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    #[serde(default)]
    pub delta: Option<ChatDelta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatDelta {
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)] pub prompt_tokens: u64,
    #[serde(default)] pub completion_tokens: u64,
    #[serde(default)] pub total_tokens: u64,
}

// ---------- /v1/runs (richer lifecycle) ----------

#[derive(Debug, Clone, Serialize)]
pub struct RunRequest {
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_history: Option<Vec<ChatMessage>>,
    /// Optional upstream model id. When set, the gateway routes this run to the
    /// matching credential_pool entry (e.g. `claude-opus-4-7`); when omitted
    /// the gateway picks its default model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional reasoning-effort hint (`minimal | low | medium | high`, Codex
    /// CLI parity). Forwarded to the gateway so it can tune the upstream
    /// reasoning model. Omitted from the wire entirely when `None`, so a request
    /// that doesn't set it is byte-for-byte unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Multi-provider isolation: when set, the gateway clones the Gitea repo and
    /// runs this agent in its own git worktree (branch `cortex/<run>/<provider>`)
    /// so parallel providers editing one project never collide. Honored by the
    /// deployed `/v1/runs` change (see gateway-integration/DEPLOYED-runs-cwd.md).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cortex_worktree: Option<CortexWorktree>,
}

/// Selects the server-side worktree a run executes in. `owner`/`repo` name the
/// Gitea project the gateway clones; `provider` keys the per-lane branch + dir.
#[derive(Debug, Clone, Serialize)]
pub struct CortexWorktree {
    pub owner: String,
    pub repo: String,
    pub provider: String,
}

#[derive(Debug, Deserialize)]
pub struct RunCreated {
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

impl RunCreated {
    pub fn effective_id(&self) -> Option<String> {
        self.run_id.clone().or_else(|| self.id.clone())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResponseBody {
    pub choice: String,
    /// Optional override for the tool call's args. When the UI lets the user
    /// edit a `bash`/`shell` command before approving, the edited payload is
    /// threaded through here so the gateway can substitute it server-side instead
    /// of running the original.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_payload: Option<serde_json::Value>,
    /// Subset of hunk indices to apply for diff-shaped approvals. `None`
    /// means "apply the whole patch" (legacy behavior); an explicit empty
    /// vec means "apply nothing" — the UI should send `deny` in that case
    /// rather than approve with `[]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_hunks: Option<Vec<u32>>,
}

// ---------- Streaming items ----------

#[derive(Debug, Clone)]
pub enum StreamItem {
    Delta(String),
    Done { usage: Option<Usage> },
}

#[derive(Debug, Clone)]
pub enum RunStreamItem {
    Started { run_id: String },
    Delta(String),
    Reasoning(String),
    ToolStarted { tool: String, preview: Option<String> },
    ToolCompleted { tool: String, duration_s: f64, error: bool },
    ApprovalRequest {
        tool: Option<String>,
        preview: Option<String>,
        choices: Vec<String>,
        raw: serde_json::Value,
    },
    ApprovalResponded { choice: String },
    Status(String),
    Done,
    Raw(serde_json::Value),
}

impl GatewayClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        // When embedded Tailscale is enabled + connected, route gateway traffic
        // through the local SOCKS5 proxy (socks5h://) so home hosts resolve +
        // tunnel over the tailnet. No-op otherwise (behavior unchanged).
        let builder = crate::tailscale::maybe_tailscale_proxy(
            reqwest::Client::builder().timeout(std::time::Duration::from_secs(600)),
        );
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            http: builder.build().expect("reqwest client"),
        }
    }

    pub async fn list_models(&self) -> anyhow::Result<ModelList> {
        let res = self
            .http
            .get(format!("{}/v1/models", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?
            .json::<ModelList>()
            .await?;
        Ok(res)
    }

    pub async fn capabilities(&self) -> anyhow::Result<serde_json::Value> {
        let res = self
            .http
            .get(format!("{}/v1/capabilities", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        Ok(res)
    }

    pub async fn health(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Stream /v1/chat/completions. Used as the basic fallback.
    pub async fn chat_completion_stream(
        &self,
        req: ChatCompletionRequest,
        tx: mpsc::Sender<StreamItem>,
    ) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Accept", "text/event-stream")
            .json(&req)
            .send()
            .await?
            .error_for_status()?;

        let mut stream = response.bytes_stream().eventsource();
        let mut final_usage: Option<Usage> = None;

        while let Some(event) = stream.next().await {
            let Ok(event) = event else { continue };
            if event.data == "[DONE]" { break; }
            let chunk: ChatCompletionChunk = match serde_json::from_str(&event.data) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Some(u) = chunk.usage { final_usage = Some(u); }
            for choice in chunk.choices {
                if let Some(d) = choice.delta.and_then(|d| d.content) {
                    if !d.is_empty() {
                        let _ = tx.send(StreamItem::Delta(d)).await;
                    }
                }
            }
        }
        let _ = tx.send(StreamItem::Done { usage: final_usage }).await;
        Ok(())
    }

    /// Start a gateway agent run.
    pub async fn start_run(
        &self,
        req: RunRequest,
        session_id: Option<&str>,
        session_key: Option<&str>,
    ) -> anyhow::Result<String> {
        let mut builder = self
            .http
            .post(format!("{}/v1/runs", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&req);
        // Back-compat: these are wire-protocol request headers the deployed
        // gateway server reads by name. They are part of the on-the-wire
        // contract with the existing homelab service, NOT user-facing branding,
        // so the `X-Hermes-*` names are kept to avoid breaking the running
        // backend (rename only once the server side is updated in lockstep).
        if let Some(sid) = session_id {
            builder = builder.header("X-Hermes-Session-Id", sid);
        }
        if let Some(skey) = session_key {
            builder = builder.header("X-Hermes-Session-Key", skey);
        }
        let res = builder.send().await?.error_for_status()?;
        let created: RunCreated = res.json().await?;
        created
            .effective_id()
            .ok_or_else(|| anyhow::anyhow!("gateway did not return a run_id"))
    }

    /// Build a `/v1/runs/{run_id}/{suffix}` URL with `run_id` percent-encoded
    /// as a path segment, so it cannot alter the request path.
    fn run_url(&self, run_id: &str, suffix: &str) -> anyhow::Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base_url)?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("gateway base_url cannot be a base"))?;
            segments.extend(["v1", "runs", run_id]);
            segments.extend(suffix.split('/'));
        }
        Ok(url)
    }

    /// Subscribe to SSE event stream for a run.
    pub async fn run_event_stream(
        &self,
        run_id: &str,
        tx: mpsc::Sender<RunStreamItem>,
    ) -> anyhow::Result<()> {
        let response = self
            .http
            .get(self.run_url(run_id, "events")?)
            .bearer_auth(&self.api_key)
            .header("Accept", "text/event-stream")
            .send()
            .await?
            .error_for_status()?;

        let mut stream = response.bytes_stream().eventsource();
        let _ = tx.send(RunStreamItem::Started { run_id: run_id.into() }).await;

        while let Some(event) = stream.next().await {
            let Ok(event) = event else { continue };
            if event.data == "[DONE]" { break; }
            let parsed: serde_json::Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let item = map_event(&parsed);
            let finished = matches!(item, RunStreamItem::Done);
            let _ = tx.send(item).await;
            if finished { break; }
        }
        Ok(())
    }

    pub async fn approve_run(
        &self,
        run_id: &str,
        choice: &str,
        edited_payload: Option<serde_json::Value>,
        accepted_hunks: Option<Vec<u32>>,
    ) -> anyhow::Result<serde_json::Value> {
        let body = ApprovalResponseBody {
            choice: choice.into(),
            edited_payload,
            accepted_hunks,
        };
        let res = self
            .http
            .post(self.run_url(run_id, "approval")?)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        Ok(res)
    }

    pub async fn stop_run(&self, run_id: &str) -> anyhow::Result<()> {
        self.http
            .post(self.run_url(run_id, "stop")?)
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

fn map_event(v: &serde_json::Value) -> RunStreamItem {
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    match event {
        "message.delta" => {
            let delta = v.get("delta").and_then(|d| d.as_str())
                .or_else(|| v.get("content").and_then(|c| c.as_str()))
                .or_else(|| v.get("text").and_then(|t| t.as_str()))
                .unwrap_or("");
            RunStreamItem::Delta(delta.to_string())
        }
        "reasoning.available" => {
            let t = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            RunStreamItem::Reasoning(t.to_string())
        }
        "tool.started" => RunStreamItem::ToolStarted {
            tool: v.get("tool").and_then(|t| t.as_str()).unwrap_or("tool").to_string(),
            preview: v.get("preview").and_then(|p| p.as_str()).map(|s| s.to_string()),
        },
        "tool.completed" => RunStreamItem::ToolCompleted {
            tool: v.get("tool").and_then(|t| t.as_str()).unwrap_or("tool").to_string(),
            duration_s: v.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0),
            error: v.get("error").and_then(|e| e.as_bool()).unwrap_or(false),
        },
        "approval.request" => RunStreamItem::ApprovalRequest {
            tool: v.get("tool").and_then(|t| t.as_str()).map(|s| s.to_string()),
            preview: v.get("preview").and_then(|p| p.as_str()).map(|s| s.to_string()),
            choices: v.get("choices")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["once".into(), "session".into(), "always".into(), "deny".into()]),
            raw: v.clone(),
        },
        "approval.responded" => RunStreamItem::ApprovalResponded {
            choice: v.get("choice").and_then(|c| c.as_str()).unwrap_or("").to_string(),
        },
        "run.completed" | "run.finished" | "done" => RunStreamItem::Done,
        "run.status" | "status" => RunStreamItem::Status(
            v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        ),
        _ => RunStreamItem::Raw(v.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_run_req() -> RunRequest {
        RunRequest {
            input: "hi".into(),
            instructions: None,
            previous_response_id: None,
            conversation_history: None,
            model: None,
            reasoning_effort: None,
            cortex_worktree: None,
        }
    }

    #[test]
    fn run_request_serializes_reasoning_effort_when_set() {
        let req = RunRequest { reasoning_effort: Some("high".into()), ..base_run_req() };
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v.get("reasoning_effort").and_then(|x| x.as_str()), Some("high"));
        assert_eq!(v.get("input").and_then(|x| x.as_str()), Some("hi"));
    }

    #[test]
    fn run_request_omits_reasoning_effort_when_none() {
        // A request that doesn't set the field must be byte-for-byte unchanged
        // on the wire (no `reasoning_effort` key at all), so existing gateway
        // behavior is untouched when nobody opts in.
        let req = base_run_req();
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert!(v.get("reasoning_effort").is_none(), "field must be omitted when None");
    }
}
