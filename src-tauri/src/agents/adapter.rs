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
