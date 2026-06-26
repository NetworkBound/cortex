//! AG-UI Protocol server module.
//!
//! Exposes Cortex (specifically the gateway adapter event stream) as an
//! AG-UI Protocol server, so external AG-UI clients (CopilotKit, ag-ui-dojo,
//! custom React apps using `@ag-ui-protocol/client`) can drive Cortex runs.
//!
//! Spec: <https://github.com/ag-ui-protocol/ag-ui> /
//!        <https://docs.ag-ui.com> — see `docs/research/AG-UI-INTEGRATION.md`.
//!
//! # Wiring instructions
//!
//! To enable this module the following must be added in the indicated files:
//!
//! 1. `src-tauri/src/lib.rs` — add at the top with the other `pub mod`s:
//!    ```ignore
//!    pub mod agui;
//!    ```
//!
//! 2. `src-tauri/src/commands/mod.rs` — register the new command module:
//!    ```ignore
//!    pub mod agui;
//!    ```
//!
//! 3. `src-tauri/src/lib.rs` `invoke_handler!` macro — add the two new
//!    command entries:
//!    ```ignore
//!    commands::agui::start_agui_server,
//!    commands::agui::stop_agui_server,
//!    ```
//!
//! 4. `src-tauri/Cargo.toml` already contains (added in this change):
//!    ```toml
//!    axum = "0.7"
//!    tower-http = { version = "0.6", features = ["cors"] }
//!    ```
//!
//! # Module layout
//!
//! - [`translator`] — maps Cortex [`AgentEvent`] values into AG-UI event JSON.
//! - [`server`] — `axum`-based HTTP/SSE server bound to `127.0.0.1:8643` by
//!   default. Off until the user calls `start_agui_server`.
//!
//! # Status
//!
//! First-pass implementation. `POST /agui/run` is a stub that returns a
//! single SSE event acknowledging the request — actual gateway run dispatch
//! is wired in a follow-up iteration.

pub mod server;
pub mod translator;

use serde::{Deserialize, Serialize};

/// Default bind address for the AG-UI server. `127.0.0.1` only — never expose
/// publicly without an auth layer in front (the protocol defines none).
pub const DEFAULT_BIND: &str = "127.0.0.1:8643";

/// Protocol version this server advertises in `/agui/health`. The AG-UI spec
/// itself does not define a `protocolVersion` field on the wire, so this is
/// purely informational and reflects Cortex's implementation revision.
pub const PROTOCOL_VERSION: &str = "0.1";

// ---------------------------------------------------------------------------
// AG-UI input types (subset — we accept these, the rest are ignored for now).
// ---------------------------------------------------------------------------

/// Wire shape posted by AG-UI clients to `POST /agui/run`.
///
/// Mirrors the TypeScript `RunAgentInput` type from the AG-UI spec. Fields we
/// don't yet consume (`tools`, `context`, `state`, `forwardedProps`) are kept
/// as `serde_json::Value` to avoid rejecting valid client payloads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAgentInput {
    pub thread_id: String,
    pub run_id: String,
    #[serde(default)]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub state: serde_json::Value,
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub tools: Vec<serde_json::Value>,
    #[serde(default)]
    pub context: Vec<serde_json::Value>,
    #[serde(default)]
    pub forwarded_props: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AG-UI event types — outbound wire shapes.
// ---------------------------------------------------------------------------
//
// All AG-UI events extend a common base of `{ type, timestamp?, rawEvent? }`
// with `type` being a SCREAMING_SNAKE_CASE discriminator. We model the subset
// the translator emits today (additional event types can be added without
// breaking existing ones because we use `#[serde(tag = "type")]`).

/// Top-level AG-UI event enum. Serializes with a `type` discriminator using
/// the spec's SCREAMING_SNAKE_CASE constants.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgUiEvent {
    #[serde(rename = "RUN_STARTED")]
    RunStarted(RunStarted),
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart(TextMessageStart),
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent(TextMessageContent),
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd(TextMessageEnd),
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart(ToolCallStart),
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs(ToolCallArgs),
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd(ToolCallEnd),
    #[serde(rename = "ERROR")]
    Error(ErrorEvent),
    #[serde(rename = "RUN_FINISHED")]
    RunFinished(RunFinished),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStarted {
    pub thread_id: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMessageStart {
    pub message_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMessageContent {
    pub message_id: String,
    pub delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMessageEnd {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallStart {
    pub tool_call_id: String,
    pub tool_call_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallArgs {
    pub tool_call_id: String,
    /// Args delta as a JSON-encoded string (AG-UI streams args as opaque
    /// string deltas — clients accumulate then JSON-parse).
    pub delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEnd {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEvent {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFinished {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

impl AgUiEvent {
    /// Encode this event as a single SSE frame (`data: <json>\n\n`).
    ///
    /// AG-UI's SSE transport puts a JSON-encoded event in the `data:` field of
    /// each frame. No `event:` line is used — the event type is carried inside
    /// the JSON via the `type` discriminator.
    pub fn into_sse_event(&self) -> String {
        // serde_json::to_string never panics on our owned, well-typed values;
        // fall back to a CUSTOM-shaped error envelope if it ever did.
        let body = serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                r#"{{"type":"CUSTOM","name":"agui.encode_error","value":{}}}"#,
                serde_json::Value::String(e.to_string())
            )
        });
        format!("data: {body}\n\n")
    }
}

/// Convenience: build a small `RUN_STARTED` event for an ad-hoc thread/run.
#[allow(dead_code)]
pub(crate) fn run_started(thread_id: impl Into<String>, run_id: impl Into<String>) -> AgUiEvent {
    AgUiEvent::RunStarted(RunStarted {
        thread_id: thread_id.into(),
        run_id: run_id.into(),
        timestamp: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_started_serializes_with_screaming_snake_type() {
        let ev = AgUiEvent::RunStarted(RunStarted {
            thread_id: "t1".into(),
            run_id: "r1".into(),
            timestamp: None,
        });
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains(r#""type":"RUN_STARTED""#), "got {s}");
        assert!(s.contains(r#""threadId":"t1""#), "got {s}");
        assert!(s.contains(r#""runId":"r1""#), "got {s}");
    }

    #[test]
    fn into_sse_event_uses_data_prefix_and_double_newline() {
        let ev = AgUiEvent::TextMessageContent(TextMessageContent {
            message_id: "m1".into(),
            delta: "hi".into(),
            timestamp: None,
        });
        let frame = ev.into_sse_event();
        assert!(frame.starts_with("data: "), "got {frame:?}");
        assert!(frame.ends_with("\n\n"), "got {frame:?}");
    }
}
