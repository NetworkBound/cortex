//! Shared state for the mobile server handlers + the WebSocket fan-out channel.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::app_state::AppState;
use crate::observability::tracing_store::TracingStore;

/// Capacity of the broadcast channel that fans streaming events out to every
/// connected WebSocket. Generous because a single chat run produces token
/// deltas at LLM speed; a slow/absent subscriber lags and drops old frames
/// (handled in [`super::ws`]) rather than back-pressuring the producer.
const BROADCAST_CAPACITY: usize = 1024;

/// An event published by a POST handler and forwarded to every `/ws` client.
///
/// Tagged enum: serializes to `{ "type": "...", ... }`. The mobile SPA listens
/// on a single WebSocket and switches on `type`. Every variant carries the
/// `run_id` (or `session_id`) the POST endpoint returned, so a client can
/// correlate frames to the run it started.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MobileEvent {
    // ── chat stream (bridged from `AgentEvent`) ───────────────────────────
    /// A chat run started.
    ChatStarted { run_id: String, session_id: String },
    /// A streamed assistant token delta.
    ChatToken { run_id: String, delta: String },
    /// A streamed reasoning/thinking delta.
    ChatReasoning { run_id: String, text: String },
    /// The agent invoked a tool.
    ChatToolCall { run_id: String, name: String, preview: Option<String> },
    /// A tool finished.
    ChatToolResult { run_id: String, name: String, ok: bool, summary: String },
    /// The agent edited a file.
    ChatFileEdit { run_id: String, path: String, lines_changed: i64 },
    /// The agent is requesting approval for an action; mirrors a pending
    /// approval the client can resolve via `POST /api/approvals/{id}`.
    ChatApproval {
        run_id: String,
        tool: Option<String>,
        preview: Option<String>,
        choices: Vec<String>,
    },
    /// An approval was resolved (by this client or another).
    ChatApprovalResolved { run_id: String, choice: String },
    /// The chat run finished.
    ChatDone { run_id: String, total_tokens: Option<u64> },
    /// A chat run errored.
    ChatError { run_id: String, message: String },

    // ── ultimate stream (bridged from `UltEvent`) ─────────────────────────
    /// A wrapped ultimate-orchestrator event. `event` is the verbatim
    /// serialized [`crate::orchestrator::ultimate::UltEvent`] (itself a
    /// `{ "type": ... }` tagged enum), so the SPA can switch on
    /// `event.type` for fine-grained ultimate progress.
    Ultimate { run_id: String, event: serde_json::Value },
    /// The ultimate run finished; carries the final result payload.
    UltimateDone { run_id: String, result: serde_json::Value },
    /// An ultimate run errored.
    UltimateError { run_id: String, message: String },
}

/// A single pending approval surfaced by a chat run, awaiting a client decision.
#[derive(Debug, Clone, Serialize)]
pub struct PendingApproval {
    pub id: String,
    pub run_id: String,
    pub tool: Option<String>,
    pub preview: Option<String>,
    pub choices: Vec<String>,
    /// The raw approval request as emitted by the agent (opaque to the client,
    /// echoed back when resolving so the bridge can match it up).
    pub request: serde_json::Value,
}

/// State cloned into every axum handler.
#[derive(Clone)]
pub struct MobileState {
    /// The shared agent registry + config (same handle the desktop app uses).
    pub app: AppState,
    /// Tracing store the ultimate orchestrator records into.
    pub store: TracingStore,
    /// Fan-out channel: POST handlers publish, `/ws` subscribers forward.
    pub events: broadcast::Sender<MobileEvent>,
    /// Pending approvals keyed by approval id, populated as chat runs surface
    /// `ApprovalRequest`s and drained when resolved.
    pub approvals: Arc<Mutex<Vec<PendingApproval>>>,
}

impl MobileState {
    pub fn new(app: AppState, store: TracingStore) -> Self {
        let (events, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            app,
            store,
            events,
            approvals: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Publish an event to all connected WebSocket clients. Returns the number
    /// of receivers; `Err` (no subscribers) is ignored — a run with no live
    /// client is still valid (the client may connect mid-run).
    pub fn publish(&self, ev: MobileEvent) {
        let _ = self.events.send(ev);
    }
}
