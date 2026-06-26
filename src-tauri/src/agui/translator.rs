//! Cortex [`AgentEvent`] → AG-UI event translator.
//!
//! See `docs/research/AG-UI-INTEGRATION.md` §5 for the full mapping table.
//! This iteration is *stateful*: the same [`TranslatorState`] is threaded
//! through every event for one run so that:
//!
//! - All `Token` deltas for a single assistant message share one
//!   `messageId` (only the first delta emits `TEXT_MESSAGE_START`).
//! - `Done` correctly closes any still-open text message before emitting
//!   `RUN_FINISHED`.
//! - Tool call lifecycles (`ToolCall` → `ToolResult`) share a stable
//!   `toolCallId` keyed on tool name.
//!
//! # Wiring
//!
//! Referenced as `pub mod translator;` from `agui/mod.rs`. The server creates
//! one [`TranslatorState`] per AG-UI run and feeds it Cortex events one at a
//! time via [`translate`].
//!
//! # Mapping
//!
//! | Cortex                       | AG-UI                                              |
//! |------------------------------|----------------------------------------------------|
//! | `Started`                    | `RUN_STARTED`                                      |
//! | `Token { delta }` (1st)      | `TEXT_MESSAGE_START` + `TEXT_MESSAGE_CONTENT`      |
//! | `Token { delta }` (subseq.)  | `TEXT_MESSAGE_CONTENT`                             |
//! | `Reasoning { .. }`           | none (buffered/dropped — UX TBD)                   |
//! | `ToolCall { name, args, .. }`| `TOOL_CALL_START` + `TOOL_CALL_ARGS`               |
//! | `ToolResult { name, ok }`    | `TOOL_CALL_END`                                    |
//! | `FileEdit { .. }`            | synthetic `TOOL_CALL_START` + `TOOL_CALL_END`      |
//! | `ApprovalRequest { .. }`     | synthetic `TOOL_CALL_START` (left open)            |
//! | `ApprovalResolved { run_id }`| `TOOL_CALL_END` for the call with the same `run_id`|
//! | `Error { message }`          | `ERROR`                                            |
//! | `Done`                       | (closes open text) + `RUN_FINISHED`                |

use std::collections::HashMap;

use ulid::Ulid;

use crate::agents::adapter::AgentEvent;

use super::{
    AgUiEvent, ErrorEvent, RunFinished, RunStarted, TextMessageContent, TextMessageEnd,
    TextMessageStart, ToolCallArgs, ToolCallEnd, ToolCallStart,
};

/// Per-run translator state. One instance lives for the duration of a single
/// AG-UI run on the server side.
#[derive(Debug, Default)]
pub struct TranslatorState {
    /// Optional pinned thread id for this run (from AG-UI input). Falls back
    /// to `run_id` when not set so RUN_STARTED always has both fields.
    pub thread_id: Option<String>,
    /// Pinned run id (from AG-UI input). When absent we use whatever the
    /// agent emits with its `Started` event, or mint a fresh ULID.
    pub run_id: Option<String>,
    /// `messageId` of the currently-open assistant text message, if any.
    /// Cleared by `Done` (after emitting `TEXT_MESSAGE_END`).
    pub current_message_id: Option<String>,
    /// Open tool calls keyed by tool name → FIFO queue of toolCallIds. We key
    /// by name because the Cortex `AgentEvent` channel doesn't surface a call
    /// id. A queue (rather than a single id) is used so that if a second call
    /// with the same name starts before the first's result arrives, neither
    /// id is leaked: results are correlated to starts in FIFO order.
    pub open_tool_calls: HashMap<String, Vec<String>>,
}

impl TranslatorState {
    /// Record a newly-started tool call id for `name`, appending to that
    /// name's FIFO queue so concurrent same-named calls don't clobber.
    fn push_open_tool_call(&mut self, name: &str, id: String) {
        self.open_tool_calls
            .entry(name.to_string())
            .or_default()
            .push(id);
    }

    /// Pop the oldest open tool call id for `name` (FIFO), removing the queue
    /// entirely when it becomes empty. Returns `None` if no call is open.
    fn pop_open_tool_call(&mut self, name: &str) -> Option<String> {
        let queue = self.open_tool_calls.get_mut(name)?;
        let id = if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        };
        if queue.is_empty() {
            self.open_tool_calls.remove(name);
        }
        id
    }
}

impl TranslatorState {
    pub fn new(thread_id: Option<String>, run_id: Option<String>) -> Self {
        Self {
            thread_id,
            run_id,
            current_message_id: None,
            open_tool_calls: HashMap::new(),
        }
    }

    fn effective_run_id(&mut self, fallback: Option<&String>) -> String {
        if let Some(r) = &self.run_id {
            return r.clone();
        }
        if let Some(r) = fallback {
            self.run_id = Some(r.clone());
            return r.clone();
        }
        let fresh = Ulid::new().to_string();
        self.run_id = Some(fresh.clone());
        fresh
    }

    fn effective_thread_id(&self, run_id: &str) -> String {
        self.thread_id.clone().unwrap_or_else(|| run_id.to_string())
    }
}

/// Translate one Cortex [`AgentEvent`] into zero or more AG-UI events,
/// mutating `state` so that subsequent calls produce coherent output.
pub fn translate(state: &mut TranslatorState, event: &AgentEvent) -> Vec<AgUiEvent> {
    match event {
        AgentEvent::Started { run_id, .. } => {
            let rid = state.effective_run_id(run_id.as_ref());
            let tid = state.effective_thread_id(&rid);
            vec![AgUiEvent::RunStarted(RunStarted {
                thread_id: tid,
                run_id: rid,
                timestamp: Some(now_ms()),
            })]
        }

        AgentEvent::Token { delta } => {
            let mut out = Vec::with_capacity(2);
            let msg_id = if let Some(id) = &state.current_message_id {
                id.clone()
            } else {
                let id = Ulid::new().to_string();
                state.current_message_id = Some(id.clone());
                out.push(AgUiEvent::TextMessageStart(TextMessageStart {
                    message_id: id.clone(),
                    role: "assistant".to_string(),
                    timestamp: Some(now_ms()),
                }));
                id
            };
            out.push(AgUiEvent::TextMessageContent(TextMessageContent {
                message_id: msg_id,
                delta: delta.clone(),
                timestamp: Some(now_ms()),
            }));
            out
        }

        // Reasoning is currently buffered/dropped — AG-UI clients treat
        // chain-of-thought differently per UI. Surface as nothing for now.
        AgentEvent::Reasoning { .. } => Vec::new(),

        AgentEvent::ToolCall { name, args, .. } => {
            let id = Ulid::new().to_string();
            state.push_open_tool_call(name, id.clone());
            vec![
                AgUiEvent::ToolCallStart(ToolCallStart {
                    tool_call_id: id.clone(),
                    tool_call_name: name.clone(),
                    parent_message_id: state.current_message_id.clone(),
                    timestamp: Some(now_ms()),
                }),
                AgUiEvent::ToolCallArgs(ToolCallArgs {
                    tool_call_id: id,
                    delta: serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                    timestamp: Some(now_ms()),
                }),
            ]
        }

        AgentEvent::ToolResult { name, ok, .. } => {
            if let Some(id) = state.pop_open_tool_call(name) {
                vec![AgUiEvent::ToolCallEnd(ToolCallEnd {
                    tool_call_id: id,
                    success: Some(*ok),
                    timestamp: Some(now_ms()),
                })]
            } else {
                // Result without a matching start — emit a synthetic
                // start/end pair so clients still see the tool happened.
                let id = Ulid::new().to_string();
                vec![
                    AgUiEvent::ToolCallStart(ToolCallStart {
                        tool_call_id: id.clone(),
                        tool_call_name: name.clone(),
                        parent_message_id: state.current_message_id.clone(),
                        timestamp: Some(now_ms()),
                    }),
                    AgUiEvent::ToolCallEnd(ToolCallEnd {
                        tool_call_id: id,
                        success: Some(*ok),
                        timestamp: Some(now_ms()),
                    }),
                ]
            }
        }

        AgentEvent::FileEdit { path, lines_changed } => {
            // Surface file edits as a synthetic `file_edit` tool call so they
            // show up in AG-UI clients that visualize tool activity.
            let id = Ulid::new().to_string();
            let args = serde_json::json!({
                "path": path.display().to_string(),
                "lines_changed": lines_changed,
            });
            vec![
                AgUiEvent::ToolCallStart(ToolCallStart {
                    tool_call_id: id.clone(),
                    tool_call_name: "file_edit".to_string(),
                    parent_message_id: state.current_message_id.clone(),
                    timestamp: Some(now_ms()),
                }),
                AgUiEvent::ToolCallArgs(ToolCallArgs {
                    tool_call_id: id.clone(),
                    delta: args.to_string(),
                    timestamp: Some(now_ms()),
                }),
                AgUiEvent::ToolCallEnd(ToolCallEnd {
                    tool_call_id: id,
                    success: Some(true),
                    timestamp: Some(now_ms()),
                }),
            ]
        }

        AgentEvent::ApprovalRequest { run_id, tool, .. } => {
            // Key the synthetic call on the approval's `run_id` so that
            // `ApprovalResolved` (which carries the same `run_id`) can close
            // exactly the matching call, even with several outstanding. The
            // human-facing tool name still embeds the underlying tool.
            let synth_name = approval_key(run_id);
            let id = Ulid::new().to_string();
            state.push_open_tool_call(&synth_name, id.clone());
            vec![AgUiEvent::ToolCallStart(ToolCallStart {
                tool_call_id: id,
                tool_call_name: format!(
                    "approval:{}",
                    tool.as_deref().unwrap_or("unknown")
                ),
                parent_message_id: state.current_message_id.clone(),
                timestamp: Some(now_ms()),
            })]
        }

        AgentEvent::ApprovalResolved { run_id, .. } => {
            // Close only the approval tool call(s) for *this* `run_id`,
            // matching the synthetic key minted in `ApprovalRequest`. Other
            // outstanding approvals are left open.
            let key = approval_key(run_id);
            let ids = state.open_tool_calls.remove(&key).unwrap_or_default();
            ids.into_iter()
                .map(|id| {
                    AgUiEvent::ToolCallEnd(ToolCallEnd {
                        tool_call_id: id,
                        success: Some(true),
                        timestamp: Some(now_ms()),
                    })
                })
                .collect()
        }

        AgentEvent::Error { message } => vec![AgUiEvent::Error(ErrorEvent {
            message: message.clone(),
            code: None,
            timestamp: Some(now_ms()),
        })],

        AgentEvent::Done { .. } => {
            let mut out = Vec::with_capacity(2);
            if let Some(id) = state.current_message_id.take() {
                out.push(AgUiEvent::TextMessageEnd(TextMessageEnd {
                    message_id: id,
                    timestamp: Some(now_ms()),
                }));
            }
            out.push(AgUiEvent::RunFinished(RunFinished {
                outcome: None,
                timestamp: Some(now_ms()),
            }));
            out
        }
    }
}

/// Internal `open_tool_calls` key for an approval, keyed on its `run_id` so a
/// resolution closes exactly the matching request rather than every open
/// approval. Kept distinct from the client-facing `approval:<tool>` name.
fn approval_key(run_id: &str) -> String {
    format!("approval-run:{run_id}")
}

/// Current epoch milliseconds. Helper kept private so the units are
/// consistent across every emitted event.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_emits_run_started_with_pinned_ids() {
        let mut s = TranslatorState::new(Some("thread-1".into()), Some("run-1".into()));
        let ev = AgentEvent::Started {
            agent_id: "gateway".into(),
            run_id: Some("ignored".into()),
        };
        let out = translate(&mut s, &ev);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgUiEvent::RunStarted(r) => {
                assert_eq!(r.thread_id, "thread-1");
                assert_eq!(r.run_id, "run-1");
            }
            other => panic!("expected RunStarted, got {other:?}"),
        }
    }

    #[test]
    fn first_token_emits_start_plus_content_then_only_content() {
        let mut s = TranslatorState::default();
        let out1 = translate(&mut s, &AgentEvent::Token { delta: "Hel".into() });
        let out2 = translate(&mut s, &AgentEvent::Token { delta: "lo!".into() });
        assert_eq!(out1.len(), 2, "first token should emit START + CONTENT");
        assert!(matches!(out1[0], AgUiEvent::TextMessageStart(_)));
        assert!(matches!(out1[1], AgUiEvent::TextMessageContent(_)));
        assert_eq!(out2.len(), 1, "subsequent token should emit only CONTENT");
        assert!(matches!(out2[0], AgUiEvent::TextMessageContent(_)));

        // message_id must be stable across all three events.
        let id0 = match &out1[0] {
            AgUiEvent::TextMessageStart(m) => m.message_id.clone(),
            _ => unreachable!(),
        };
        let id1 = match &out1[1] {
            AgUiEvent::TextMessageContent(m) => m.message_id.clone(),
            _ => unreachable!(),
        };
        let id2 = match &out2[0] {
            AgUiEvent::TextMessageContent(m) => m.message_id.clone(),
            _ => unreachable!(),
        };
        assert_eq!(id0, id1);
        assert_eq!(id1, id2);
    }

    #[test]
    fn tool_call_then_result_share_id() {
        let mut s = TranslatorState::default();
        let out_start = translate(
            &mut s,
            &AgentEvent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"cmd": "ls"}),
                preview: None,
            },
        );
        let out_end = translate(
            &mut s,
            &AgentEvent::ToolResult {
                name: "shell".into(),
                ok: true,
                summary: String::new(),
                duration_ms: Some(12),
            },
        );

        assert_eq!(out_start.len(), 2);
        let start_id = match &out_start[0] {
            AgUiEvent::ToolCallStart(t) => t.tool_call_id.clone(),
            _ => panic!("expected ToolCallStart"),
        };
        let args_id = match &out_start[1] {
            AgUiEvent::ToolCallArgs(t) => t.tool_call_id.clone(),
            _ => panic!("expected ToolCallArgs"),
        };
        assert_eq!(start_id, args_id);

        assert_eq!(out_end.len(), 1);
        let end_id = match &out_end[0] {
            AgUiEvent::ToolCallEnd(t) => t.tool_call_id.clone(),
            _ => panic!("expected ToolCallEnd"),
        };
        assert_eq!(start_id, end_id);
    }

    #[test]
    fn done_closes_open_text_then_emits_run_finished() {
        let mut s = TranslatorState::default();
        let _ = translate(&mut s, &AgentEvent::Token { delta: "hi".into() });
        let out = translate(
            &mut s,
            &AgentEvent::Done { total_tokens: None, run_id: None },
        );
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], AgUiEvent::TextMessageEnd(_)));
        assert!(matches!(out[1], AgUiEvent::RunFinished(_)));
        assert!(s.current_message_id.is_none());
    }

    #[test]
    fn error_event_is_translated() {
        let mut s = TranslatorState::default();
        let out = translate(&mut s, &AgentEvent::Error { message: "boom".into() });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgUiEvent::Error(e) => assert_eq!(e.message, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Smoke-test that a realistic event sequence produces a coherent SSE
    /// frame stream. Prints frames so test logs document the wire shape.
    #[test]
    fn full_run_sequence_translates_to_expected_sse_frames() {
        let mut s = TranslatorState::new(Some("thr-A".into()), Some("run-A".into()));
        let sequence = vec![
            AgentEvent::Started {
                agent_id: "gateway-remote".into(),
                run_id: Some("run-A".into()),
            },
            AgentEvent::Token { delta: "Hello".into() },
            AgentEvent::Token { delta: ", world".into() },
            AgentEvent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"cmd": "ls /"}),
                preview: Some("ls /".into()),
            },
            AgentEvent::ToolResult {
                name: "shell".into(),
                ok: true,
                summary: "bin etc home".into(),
                duration_ms: Some(8),
            },
            AgentEvent::Token { delta: "!".into() },
            AgentEvent::Done { total_tokens: Some(7), run_id: Some("run-A".into()) },
        ];

        let mut frames: Vec<String> = Vec::new();
        for ev in &sequence {
            for agui in translate(&mut s, ev) {
                frames.push(agui.into_sse_event());
            }
        }
        for f in &frames {
            // Visible in `cargo test -- --nocapture`. Each must be a valid
            // SSE frame: starts with `data: ` and ends with `\n\n`.
            println!("FRAME: {f:?}");
            assert!(f.starts_with("data: "), "bad frame prefix: {f:?}");
            assert!(f.ends_with("\n\n"), "bad frame suffix: {f:?}");
        }

        // Sanity: RUN_STARTED first, RUN_FINISHED last, one TEXT_MESSAGE_END.
        let joined = frames.join("");
        assert!(joined.contains(r#""type":"RUN_STARTED""#));
        assert!(joined.contains(r#""type":"TEXT_MESSAGE_START""#));
        assert!(joined.contains(r#""type":"TEXT_MESSAGE_CONTENT""#));
        assert!(joined.contains(r#""type":"TOOL_CALL_START""#));
        assert!(joined.contains(r#""type":"TOOL_CALL_ARGS""#));
        assert!(joined.contains(r#""type":"TOOL_CALL_END""#));
        assert!(joined.contains(r#""type":"TEXT_MESSAGE_END""#));
        assert!(joined.contains(r#""type":"RUN_FINISHED""#));
        assert_eq!(joined.matches(r#""type":"RUN_STARTED""#).count(), 1);
        assert_eq!(joined.matches(r#""type":"RUN_FINISHED""#).count(), 1);
        assert_eq!(joined.matches(r#""type":"TEXT_MESSAGE_END""#).count(), 1);
    }
}
