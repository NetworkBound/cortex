//! The v2 event hub: live run state + the WebSocket event vocabulary of the
//! mobile contract.
//!
//! Every chat run — started from a phone (`POST /api/v2/threads/:id/send`)
//! OR from the desktop composer — flows through `commands::chat::chat_send_with`,
//! whose [`crate::commands::chat::ChatSink`] forwards each emitted payload to
//! the global tap installed by [`V2Hub::install_tap`]. The hub turns those
//! `agent-event:<session>` payloads into [`V2Event`] frames (fanned out over
//! `/ws`), keeps the in-flight assistant message per run (so a phone that
//! connects mid-run can render what streamed so far), records pending
//! approvals, persists the finished turn for phone-originated runs, and
//! fires Web Push for approvals / run outcomes.
//!
//! Thread id == chat session id (the key of the `messages` table), so the
//! desktop and the phone see one set of conversations.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::agents::AgentEvent;
use crate::observability::tracing_store::{StoredMessage, TracingStore};

use super::webpush;

/// Fan-out capacity; a lagging phone drops old frames rather than
/// back-pressuring the run (same policy as the legacy channel).
const BROADCAST_CAPACITY: usize = 2048;

/// Finished runs kept in memory for `GET /runs` merging + late `stop`s.
const FINISHED_RUNS_CAP: usize = 200;

/// Preview length for tool args / results in the wire shape.
const PREVIEW_CHARS: usize = 240;

/// Last-message preview length on a thread row.
pub const THREAD_PREVIEW_CHARS: usize = 140;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Truncate on a char boundary with an ellipsis.
pub fn preview(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Wire shapes (contract: Message.tool_calls[], Message.approval, WS events)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    pub args_preview: String,
    /// `pending` | `approved` | `denied` | `done` | `error`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalView {
    pub id: String,
    pub tool: String,
    pub detail: String,
    pub resolved: bool,
    /// Short rendering of the tool arguments (the command / path / patch head).
    pub args_preview: String,
    /// `read` | `write` | `exec` | `network` | `unknown` — a badge hint only.
    pub risk: String,
    pub thread_id: String,
    pub run_id: String,
    #[serde(default)]
    pub choices: Vec<String>,
    pub created_ms: i64,
    /// Same instant as `created_ms` (the client's field name).
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageView {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadView {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub created_ms: i64,
    pub last_ms: i64,
    pub pending_approvals: usize,
    pub running: bool,
    pub last_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageView {
    pub id: String,
    /// `user` | `assistant` | `system`.
    pub role: String,
    pub content: String,
    pub ts_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallView>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// True for the in-flight assistant message of a running run (additive
    /// to the contract; clients may ignore it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunView {
    pub run_id: String,
    pub thread_id: String,
    pub started_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// `running` | `done` | `error` | `stopped`.
    pub status: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// One frame on `/ws`. Serialises to `{ "type": "...", ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V2Event {
    Token {
        thread_id: String,
        run_id: String,
        message_id: String,
        delta: String,
    },
    Reasoning {
        thread_id: String,
        run_id: String,
        message_id: String,
        delta: String,
    },
    ToolCall {
        thread_id: String,
        run_id: String,
        message_id: String,
        tool: ToolCallView,
    },
    ToolResult {
        thread_id: String,
        run_id: String,
        message_id: String,
        tool: ToolCallView,
    },
    ApprovalRequest {
        thread_id: String,
        run_id: String,
        approval: ApprovalView,
    },
    ApprovalResolved {
        approval_id: String,
        decision: String,
        thread_id: String,
        run_id: String,
    },
    Done {
        thread_id: String,
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<UsageView>,
    },
    Error {
        thread_id: String,
        run_id: String,
        message: String,
    },
    ThreadUpdated {
        thread: ThreadView,
    },
    Ping,
}

impl V2Event {
    /// The thread a frame belongs to (`None` for `ping`), used by the
    /// per-socket subscription filter.
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            V2Event::Token { thread_id, .. }
            | V2Event::Reasoning { thread_id, .. }
            | V2Event::ToolCall { thread_id, .. }
            | V2Event::ToolResult { thread_id, .. }
            | V2Event::ApprovalRequest { thread_id, .. }
            | V2Event::ApprovalResolved { thread_id, .. }
            | V2Event::Done { thread_id, .. }
            | V2Event::Error { thread_id, .. } => Some(thread_id),
            V2Event::ThreadUpdated { thread } => Some(&thread.id),
            V2Event::Ping => None,
        }
    }
}

/// Coarse risk classification of a tool call, for the approval badge.
pub fn risk_hint(tool: &str, detail: &str) -> &'static str {
    let t = tool.to_ascii_lowercase();
    let d = detail.to_ascii_lowercase();
    let any = |hay: &str, needles: &[&str]| needles.iter().any(|n| hay.contains(n));
    if any(
        &t,
        &[
            "bash",
            "shell",
            "exec",
            "run_command",
            "terminal",
            "command",
            "process",
        ],
    ) {
        return "exec";
    }
    if any(
        &t,
        &["fetch", "http", "web", "curl", "download", "browse", "url"],
    ) || any(&d, &["http://", "https://", "curl ", "wget "])
    {
        return "network";
    }
    if any(
        &t,
        &[
            "write", "edit", "create", "delete", "remove", "patch", "apply", "move", "rename",
            "mkdir", "rm",
        ],
    ) {
        return "write";
    }
    if any(
        &t,
        &[
            "read", "list", "search", "grep", "glob", "find", "cat", "view", "get", "ls",
        ],
    ) {
        return "read";
    }
    "unknown"
}

// ───────────────────────────────────────────────────────────────────────────
// Live run state
// ───────────────────────────────────────────────────────────────────────────

/// Who started the run — only phone-originated runs are persisted by the
/// hub (the desktop frontend records its own turns via `record_message`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOrigin {
    Mobile,
    Desktop,
}

#[derive(Debug, Clone)]
pub struct LiveRun {
    pub run_id: String,
    pub thread_id: String,
    pub message_id: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub origin: RunOrigin,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub project_root: Option<String>,
    /// Ids the pipeline assigned to the underlying agent runs (`local-<ulid>`
    /// or the gateway's) — what Stop must abort.
    pub agent_run_ids: Vec<String>,
    pub content: String,
    pub reasoning: String,
    pub tools: Vec<ToolCallView>,
    pub error: Option<String>,
    pub routing_reason: Option<String>,
    /// Agents dispatched for this turn (from `orchestrator_route`); the run is
    /// finished once every one of them has sent `Done`.
    pub expected_agents: usize,
    pub done_agents: usize,
    pub total_tokens: u64,
    pub stopped: bool,
    pub tool_seq: usize,
}

impl LiveRun {
    fn status(&self) -> &'static str {
        if self.ended_ms.is_none() {
            "running"
        } else if self.stopped {
            "stopped"
        } else if self.error.is_some() && self.content.trim().is_empty() {
            "error"
        } else {
            "done"
        }
    }

    pub fn view(&self) -> RunView {
        let cost = if self.total_tokens > 0 {
            let price = crate::pricing::lookup_price(
                self.model
                    .as_deref()
                    .unwrap_or(self.agent_id.as_deref().unwrap_or("")),
            );
            let (p, c) = crate::pricing::split_tokens(self.total_tokens);
            Some(crate::pricing::compute_usd(p, c, price))
        } else {
            None
        };
        RunView {
            run_id: self.run_id.clone(),
            thread_id: self.thread_id.clone(),
            started_ms: self.started_ms,
            ended_ms: self.ended_ms,
            status: self.status().to_string(),
            agent_id: self.agent_id.clone(),
            model: self.model.clone(),
            cost_usd: cost,
            tokens: (self.total_tokens > 0).then_some(self.total_tokens),
        }
    }

    /// The in-flight (or just-finished) assistant message.
    pub fn message(&self) -> MessageView {
        MessageView {
            id: self.message_id.clone(),
            role: "assistant".into(),
            content: self.content.clone(),
            ts_ms: self.started_ms,
            run_id: Some(self.run_id.clone()),
            tool_calls: (!self.tools.is_empty()).then(|| self.tools.clone()),
            approval: None,
            error: self.error.clone(),
            routing_reason: self.routing_reason.clone(),
            reasoning: (!self.reasoning.is_empty()).then(|| self.reasoning.clone()),
            agent_id: self.agent_id.clone(),
            pending: self.ended_ms.is_none(),
        }
    }
}

struct Inner {
    /// Active runs by run id.
    runs: HashMap<String, LiveRun>,
    /// thread id → active run id.
    by_thread: HashMap<String, String>,
    /// Recently finished runs, newest last.
    finished: VecDeque<LiveRun>,
    /// Pending + recently resolved approvals (resolved ones age out).
    approvals: Vec<ApprovalView>,
}

/// Shared between the axum handlers, the WebSocket fan-out and the chat tap.
pub struct V2Hub {
    tx: broadcast::Sender<V2Event>,
    inner: Mutex<Inner>,
    store: TracingStore,
}

impl V2Hub {
    pub fn new(store: TracingStore) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(Self {
            tx,
            inner: Mutex::new(Inner {
                runs: HashMap::new(),
                by_thread: HashMap::new(),
                finished: VecDeque::new(),
                approvals: Vec::new(),
            }),
            store,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<V2Event> {
        self.tx.subscribe()
    }

    pub fn publish(&self, ev: V2Event) {
        let _ = self.tx.send(ev);
    }

    /// Hook this hub into the chat pipeline. Idempotent per process (the
    /// first hub wins — see `chat::set_event_tap`).
    pub fn install_tap(self: &Arc<Self>) {
        let hub = Arc::clone(self);
        crate::commands::chat::set_event_tap(Arc::new(move |event: &str, payload: &Value| {
            if let Some(session) = event.strip_prefix("agent-event:") {
                hub.on_chat_payload(session, payload);
            }
        }));
    }

    // ── run lifecycle ──────────────────────────────────────────────────────

    /// Register a phone-originated run BEFORE dispatching it, so the very
    /// first streamed event already maps to it. Returns `(run_id, message_id)`.
    pub fn begin_run(
        &self,
        thread_id: &str,
        agent_id: Option<String>,
        model: Option<String>,
        project_root: Option<String>,
    ) -> (String, String) {
        let run = self.new_run(thread_id, RunOrigin::Mobile, agent_id, model, project_root);
        let ids = (run.run_id.clone(), run.message_id.clone());
        let mut g = self.inner.lock();
        g.by_thread
            .insert(thread_id.to_string(), run.run_id.clone());
        g.runs.insert(run.run_id.clone(), run);
        ids
    }

    fn new_run(
        &self,
        thread_id: &str,
        origin: RunOrigin,
        agent_id: Option<String>,
        model: Option<String>,
        project_root: Option<String>,
    ) -> LiveRun {
        let ulid = ulid::Ulid::new().to_string().to_lowercase();
        LiveRun {
            run_id: format!("run-{ulid}"),
            thread_id: thread_id.to_string(),
            message_id: format!("asst-{ulid}"),
            started_ms: now_ms(),
            ended_ms: None,
            origin,
            agent_id,
            model,
            project_root,
            agent_run_ids: Vec::new(),
            content: String::new(),
            reasoning: String::new(),
            tools: Vec::new(),
            error: None,
            routing_reason: None,
            expected_agents: 1,
            done_agents: 0,
            total_tokens: 0,
            stopped: false,
            tool_seq: 0,
        }
    }

    /// The chat tap: one `agent-event:<session>` payload from the pipeline.
    pub fn on_chat_payload(&self, session: &str, payload: &Value) {
        // Route hint: `{ type: "orchestrator_route", agents, reason }`.
        if payload.get("type").and_then(Value::as_str) == Some("orchestrator_route") {
            let agents: Vec<String> = payload
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            let mut g = self.inner.lock();
            let inner: &mut Inner = &mut g;
            let run_id = self.active_or_new(inner, session);
            if let Some(run) = inner.runs.get_mut(&run_id) {
                // A failover re-route arrives mid-run with one agent; only the
                // initial route sets the fan-out count.
                if run.done_agents == 0 && !agents.is_empty() {
                    run.expected_agents = agents.len();
                }
                if run.agent_id.is_none() {
                    run.agent_id = agents.first().cloned();
                }
                if let Some(r) = reason {
                    let merged = match run.routing_reason.take() {
                        Some(mut existing) => {
                            if !existing.contains(r.as_str()) {
                                existing.push_str(" · ");
                                existing.push_str(&r);
                            }
                            existing
                        }
                        None => r,
                    };
                    run.routing_reason = Some(merged);
                }
            }
            return;
        }
        // Hook block: `{ type: "error", message }` before dispatch.
        if payload.get("type").and_then(Value::as_str) == Some("error") {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_string();
            let finished = {
                let mut g = self.inner.lock();
                let inner: &mut Inner = &mut g;
                let run_id = self.active_or_new(inner, session);
                let run = inner.runs.get_mut(&run_id).map(|r| {
                    r.error = Some(message.clone());
                    r.ended_ms = Some(now_ms());
                    r.run_id.clone()
                });
                run.and_then(|id| self.take_finished(inner, &id))
            };
            if let Some(run) = finished {
                self.publish(V2Event::Error {
                    thread_id: run.thread_id.clone(),
                    run_id: run.run_id.clone(),
                    message,
                });
                self.finalize(run);
            }
            return;
        }
        // Agent event: `{ agent_id, event: AgentEvent }`.
        let agent_id = payload
            .get("agent_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(ev) = payload
            .get("event")
            .cloned()
            .and_then(|e| serde_json::from_value::<AgentEvent>(e).ok())
        else {
            return;
        };
        self.on_agent_event(session, agent_id, ev);
    }

    /// Active run for `session`, or a new desktop-origin one.
    fn active_or_new(&self, g: &mut Inner, session: &str) -> String {
        if let Some(id) = g.by_thread.get(session) {
            if g.runs.contains_key(id) {
                return id.clone();
            }
        }
        let run = self.new_run(session, RunOrigin::Desktop, None, None, None);
        let id = run.run_id.clone();
        g.by_thread.insert(session.to_string(), id.clone());
        g.runs.insert(id.clone(), run);
        id
    }

    /// Pop a run out of the active maps (once every agent finished).
    fn take_finished(&self, g: &mut Inner, run_id: &str) -> Option<LiveRun> {
        let run = g.runs.remove(run_id)?;
        if g.by_thread.get(&run.thread_id) == Some(&run.run_id) {
            g.by_thread.remove(&run.thread_id);
        }
        g.finished.push_back(run.clone());
        while g.finished.len() > FINISHED_RUNS_CAP {
            g.finished.pop_front();
        }
        Some(run)
    }

    fn on_agent_event(&self, session: &str, agent_id: Option<String>, ev: AgentEvent) {
        let mut g = self.inner.lock();
        let inner: &mut Inner = &mut g;
        let run_id = self.active_or_new(inner, session);
        let Some(run) = inner.runs.get_mut(&run_id) else {
            return;
        };
        if run.agent_id.is_none() {
            run.agent_id = agent_id.clone();
        }
        let thread_id = run.thread_id.clone();
        let message_id = run.message_id.clone();
        // Short run tag for tool-call ids (`run-<ulid>` → 8 ulid chars).
        let short: String = run_id.chars().skip(4).take(8).collect();
        let mut out: Vec<V2Event> = Vec::new();
        let mut finished: Option<LiveRun> = None;
        let mut push: Option<(String, webpush::Notification)> = None;
        match ev {
            AgentEvent::Started { run_id: rid, .. } => {
                if let Some(rid) = rid {
                    if !run.agent_run_ids.contains(&rid) {
                        run.agent_run_ids.push(rid);
                    }
                }
            }
            AgentEvent::Token { delta } => {
                run.content.push_str(&delta);
                out.push(V2Event::Token {
                    thread_id,
                    run_id: run_id.clone(),
                    message_id,
                    delta,
                });
            }
            AgentEvent::Reasoning { text } => {
                run.reasoning.push_str(&text);
                out.push(V2Event::Reasoning {
                    thread_id,
                    run_id: run_id.clone(),
                    message_id,
                    delta: text,
                });
            }
            AgentEvent::ToolCall {
                name,
                args,
                preview: pv,
            } => {
                run.tool_seq += 1;
                let args_preview = match pv {
                    Some(p) if !p.trim().is_empty() => preview(&p, PREVIEW_CHARS),
                    _ => preview(&args.to_string(), PREVIEW_CHARS),
                };
                let tool = ToolCallView {
                    id: format!("tc-{}-{short}", run.tool_seq),
                    name,
                    args_preview,
                    status: "pending".into(),
                    result_preview: None,
                    duration_ms: None,
                };
                run.tools.push(tool.clone());
                out.push(V2Event::ToolCall {
                    thread_id,
                    run_id: run_id.clone(),
                    message_id,
                    tool,
                });
            }
            AgentEvent::ToolResult {
                name,
                ok,
                summary,
                duration_ms,
            } => {
                let idx = run
                    .tools
                    .iter()
                    .rposition(|t| t.name == name && t.status == "pending")
                    .or_else(|| run.tools.iter().rposition(|t| t.name == name));
                let tool = match idx {
                    Some(i) => {
                        let t = &mut run.tools[i];
                        t.status = if ok { "done".into() } else { "error".into() };
                        t.result_preview = Some(preview(&summary, PREVIEW_CHARS));
                        t.duration_ms = duration_ms;
                        t.clone()
                    }
                    None => {
                        run.tool_seq += 1;
                        let t = ToolCallView {
                            id: format!("tc-{}-{short}", run.tool_seq),
                            name,
                            args_preview: String::new(),
                            status: if ok { "done".into() } else { "error".into() },
                            result_preview: Some(preview(&summary, PREVIEW_CHARS)),
                            duration_ms,
                        };
                        run.tools.push(t.clone());
                        t
                    }
                };
                out.push(V2Event::ToolResult {
                    thread_id,
                    run_id: run_id.clone(),
                    message_id,
                    tool,
                });
            }
            AgentEvent::FileEdit {
                path,
                lines_changed,
            } => {
                run.tool_seq += 1;
                let tool = ToolCallView {
                    id: format!("tc-{}-{short}", run.tool_seq),
                    name: "edit".into(),
                    args_preview: preview(&path.to_string_lossy(), PREVIEW_CHARS),
                    status: "done".into(),
                    result_preview: Some(format!("{lines_changed} lines changed")),
                    duration_ms: None,
                };
                run.tools.push(tool.clone());
                out.push(V2Event::ToolResult {
                    thread_id,
                    run_id: run_id.clone(),
                    message_id,
                    tool,
                });
            }
            AgentEvent::ApprovalRequest {
                run_id: approval_id,
                tool,
                preview: pv,
                choices,
                request,
            } => {
                let tool_name = tool.unwrap_or_else(|| "tool".to_string());
                let detail = match &pv {
                    Some(p) if !p.trim().is_empty() => p.clone(),
                    _ => request.to_string(),
                };
                let args_preview = preview(&detail, PREVIEW_CHARS);
                let approval = ApprovalView {
                    id: approval_id,
                    risk: risk_hint(&tool_name, &detail).to_string(),
                    tool: tool_name,
                    detail,
                    resolved: false,
                    args_preview,
                    thread_id: thread_id.clone(),
                    run_id: run_id.clone(),
                    choices,
                    created_ms: now_ms(),
                    ts_ms: now_ms(),
                };
                inner.approvals.retain(|a| a.id != approval.id);
                inner.approvals.push(approval.clone());
                let badge = inner.approvals.iter().filter(|a| !a.resolved).count() as u32;
                out.push(V2Event::ApprovalRequest {
                    thread_id,
                    run_id: run_id.clone(),
                    approval: approval.clone(),
                });
                push = Some((
                    format!("approval:{}", approval.id),
                    webpush::Notification {
                        event: webpush::EVENT_APPROVAL_NEEDED,
                        title: "Approval needed".into(),
                        body: format!("{}: {}", approval.tool, approval.args_preview),
                        target_id: Some(approval.id.clone()),
                        thread_id: Some(approval.thread_id.clone()),
                        run_id: Some(approval.run_id.clone()),
                        app_badge: Some(badge),
                    },
                ));
            }
            AgentEvent::ApprovalResolved {
                run_id: approval_id,
                choice,
            } => {
                let decision = if choice.eq_ignore_ascii_case("deny") {
                    "deny"
                } else {
                    "approve"
                };
                if let Some(t) = run.tools.iter_mut().rev().find(|t| t.status == "pending") {
                    t.status = if decision == "deny" {
                        "denied".into()
                    } else {
                        "approved".into()
                    };
                }
                if let Some(a) = inner.approvals.iter_mut().find(|a| a.id == approval_id) {
                    a.resolved = true;
                }
                out.push(V2Event::ApprovalResolved {
                    approval_id,
                    decision: decision.to_string(),
                    thread_id,
                    run_id: run_id.clone(),
                });
            }
            AgentEvent::Error { message } => {
                run.error = Some(message.clone());
                out.push(V2Event::Error {
                    thread_id,
                    run_id: run_id.clone(),
                    message,
                });
            }
            AgentEvent::Done { total_tokens, .. } => {
                run.total_tokens += total_tokens.unwrap_or(0);
                run.done_agents += 1;
                if run.done_agents >= run.expected_agents {
                    run.ended_ms = Some(now_ms());
                    finished = self.take_finished(inner, &run_id);
                }
            }
        }
        drop(g);
        for ev in out {
            self.publish(ev);
        }
        if let Some((key, n)) = push {
            webpush::fire_detached(key, n);
        }
        if let Some(run) = finished {
            let usage = if run.total_tokens > 0 {
                let v = run.view();
                let (i, o) = crate::pricing::split_tokens(run.total_tokens);
                Some(UsageView {
                    input_tokens: i,
                    output_tokens: o,
                    cost_usd: v.cost_usd,
                })
            } else {
                None
            };
            self.publish(V2Event::Done {
                thread_id: run.thread_id.clone(),
                run_id: run.run_id.clone(),
                usage,
            });
            self.finalize(run);
        }
    }

    /// Persist (phone-originated), announce the thread, and push.
    fn finalize(&self, run: LiveRun) {
        if run.origin == RunOrigin::Mobile {
            let now = now_ms();
            if !run.content.trim().is_empty() {
                let _ = self.store.record_message(&StoredMessage {
                    id: run.message_id.clone(),
                    session_id: run.thread_id.clone(),
                    ts: now,
                    role: "assistant".into(),
                    agent_id: run.agent_id.clone(),
                    content: run.content.clone(),
                    run_id: Some(run.run_id.clone()),
                    reasoning: (!run.reasoning.trim().is_empty()).then(|| run.reasoning.clone()),
                    project_root: run.project_root.clone(),
                });
            } else if let Some(err) = &run.error {
                let _ = self.store.record_message(&StoredMessage {
                    id: format!("err-{}", &run.message_id[5..]),
                    session_id: run.thread_id.clone(),
                    ts: now,
                    role: "system".into(),
                    agent_id: run.agent_id.clone(),
                    content: format!("⚠️ {err}"),
                    run_id: Some(run.run_id.clone()),
                    reasoning: None,
                    project_root: run.project_root.clone(),
                });
            }
        }
        if let Some(thread) = super::threads::thread_view(&self.store, self, &run.thread_id) {
            self.publish(V2Event::ThreadUpdated { thread });
        }
        if !run.stopped {
            let failed = run.error.is_some() && run.content.trim().is_empty();
            let agent = run.agent_id.clone().unwrap_or_else(|| "agent".into());
            webpush::fire_detached(
                format!("run:{}", run.run_id),
                webpush::Notification {
                    event: if failed {
                        webpush::EVENT_RUN_FAILED
                    } else {
                        webpush::EVENT_RUN_FINISHED
                    },
                    title: if failed {
                        format!("{agent} failed")
                    } else {
                        format!("{agent} finished")
                    },
                    body: if failed {
                        preview(run.error.as_deref().unwrap_or(""), PREVIEW_CHARS)
                    } else {
                        preview(&run.content, PREVIEW_CHARS)
                    },
                    target_id: Some(run.thread_id.clone()),
                    thread_id: Some(run.thread_id.clone()),
                    run_id: Some(run.run_id.clone()),
                    app_badge: None,
                },
            );
        }
    }

    /// End a run that could not be dispatched (`chat_send_with` returned
    /// `Err` before any event flowed): publish the error, persist the outcome
    /// for phone-originated runs and announce the thread.
    pub fn fail_run(&self, run_id: &str, message: &str) {
        let finished = {
            let mut g = self.inner.lock();
            let inner: &mut Inner = &mut g;
            if let Some(r) = inner.runs.get_mut(run_id) {
                r.error = Some(message.to_string());
                r.ended_ms = Some(now_ms());
            }
            self.take_finished(inner, run_id)
        };
        if let Some(run) = finished {
            self.publish(V2Event::Error {
                thread_id: run.thread_id.clone(),
                run_id: run.run_id.clone(),
                message: message.to_string(),
            });
            self.finalize(run);
        }
    }

    // ── queries used by the handlers ──────────────────────────────────────

    pub fn is_running(&self, thread_id: &str) -> bool {
        let g = self.inner.lock();
        g.by_thread
            .get(thread_id)
            .is_some_and(|id| g.runs.contains_key(id))
    }

    pub fn pending_approvals_for(&self, thread_id: &str) -> usize {
        self.inner
            .lock()
            .approvals
            .iter()
            .filter(|a| !a.resolved && a.thread_id == thread_id)
            .count()
    }

    pub fn pending_approvals(&self) -> Vec<ApprovalView> {
        self.inner
            .lock()
            .approvals
            .iter()
            .filter(|a| !a.resolved)
            .cloned()
            .collect()
    }

    pub fn approval(&self, id: &str) -> Option<ApprovalView> {
        self.inner
            .lock()
            .approvals
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    /// Mark an approval resolved (the pipeline's own `ApprovalResolved`
    /// event will follow; this makes the REST response immediate).
    pub fn mark_approval_resolved(&self, id: &str, decision: &str) -> Option<ApprovalView> {
        let mut g = self.inner.lock();
        let a = g.approvals.iter_mut().find(|a| a.id == id)?;
        a.resolved = true;
        let view = a.clone();
        drop(g);
        self.publish(V2Event::ApprovalResolved {
            approval_id: view.id.clone(),
            decision: decision.to_string(),
            thread_id: view.thread_id.clone(),
            run_id: view.run_id.clone(),
        });
        Some(view)
    }

    /// The in-flight assistant message for a thread, if a run is active.
    pub fn live_message(&self, thread_id: &str) -> Option<MessageView> {
        let g = self.inner.lock();
        let id = g.by_thread.get(thread_id)?;
        g.runs.get(id).map(LiveRun::message)
    }

    /// Active + recently finished runs (newest first), optionally per thread.
    pub fn runs(&self, thread_id: Option<&str>) -> Vec<RunView> {
        let g = self.inner.lock();
        let mut out: Vec<RunView> = g
            .runs
            .values()
            .chain(g.finished.iter())
            .filter(|r| thread_id.map_or(true, |t| r.thread_id == t))
            .map(LiveRun::view)
            .collect();
        out.sort_by(|a, b| b.started_ms.cmp(&a.started_ms));
        out
    }

    pub fn run(&self, run_id: &str) -> Option<LiveRun> {
        let g = self.inner.lock();
        g.runs
            .get(run_id)
            .cloned()
            .or_else(|| g.finished.iter().find(|r| r.run_id == run_id).cloned())
    }

    /// Mark a run stopped-by-user and hand back the agent run ids to abort.
    /// `None` when the run is unknown or already over.
    pub fn mark_stopped(&self, run_id: &str) -> Option<Vec<String>> {
        let mut g = self.inner.lock();
        let run = g.runs.get_mut(run_id)?;
        run.stopped = true;
        Some(run.agent_run_ids.clone())
    }

    /// Threads touched since `since_ms` (for WS `subscribe { since_ms }`):
    /// the ids of runs that started or ended after that instant.
    pub fn threads_active_since(&self, since_ms: i64) -> Vec<String> {
        let g = self.inner.lock();
        let mut ids: Vec<String> = g
            .runs
            .values()
            .chain(g.finished.iter())
            .filter(|r| r.started_ms >= since_ms || r.ended_ms.is_some_and(|e| e >= since_ms))
            .map(|r| r.thread_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hub() -> Arc<V2Hub> {
        V2Hub::new(TracingStore::in_memory())
    }

    fn agent(ev: AgentEvent) -> Value {
        json!({ "agent_id": "claude-cli", "event": ev })
    }

    #[test]
    fn mobile_run_streams_tokens_tools_and_persists_on_done() {
        let hub = hub();
        let mut rx = hub.subscribe();
        let (run_id, message_id) = hub.begin_run("sess-1", Some("claude-cli".into()), None, None);
        assert!(hub.is_running("sess-1"));
        hub.on_chat_payload(
            "sess-1",
            &json!({ "type": "orchestrator_route", "agents": ["claude-cli"], "reason": "explicit" }),
        );
        hub.on_chat_payload(
            "sess-1",
            &agent(AgentEvent::Started {
                agent_id: "claude-cli".into(),
                run_id: Some("local-abc".into()),
            }),
        );
        hub.on_chat_payload(
            "sess-1",
            &agent(AgentEvent::Token {
                delta: "Hel".into(),
            }),
        );
        hub.on_chat_payload("sess-1", &agent(AgentEvent::Token { delta: "lo".into() }));
        hub.on_chat_payload(
            "sess-1",
            &agent(AgentEvent::ToolCall {
                name: "read_file".into(),
                args: json!({ "path": "a.rs" }),
                preview: Some("a.rs".into()),
            }),
        );
        hub.on_chat_payload(
            "sess-1",
            &agent(AgentEvent::ToolResult {
                name: "read_file".into(),
                ok: true,
                summary: "12 lines".into(),
                duration_ms: Some(3),
            }),
        );
        // Mid-run snapshot for a late-joining phone.
        let live = hub.live_message("sess-1").unwrap();
        assert_eq!(live.content, "Hello");
        assert!(live.pending);
        assert_eq!(live.tool_calls.as_ref().unwrap()[0].status, "done");
        assert_eq!(
            hub.mark_stopped(&run_id),
            Some(vec!["local-abc".to_string()])
        );

        hub.on_chat_payload(
            "sess-1",
            &agent(AgentEvent::Done {
                total_tokens: Some(100),
                run_id: Some("local-abc".into()),
            }),
        );
        assert!(!hub.is_running("sess-1"));
        // Persisted as the assistant turn of the session.
        let msgs = hub.store.load_session_messages("sess-1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, message_id);
        assert_eq!(msgs[0].content, "Hello");
        assert_eq!(msgs[0].run_id.as_deref(), Some(run_id.as_str()));

        // Frame order + shape.
        let mut kinds = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            kinds.push(
                serde_json::to_value(&ev).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(
            kinds,
            vec![
                "token",
                "token",
                "tool_call",
                "tool_result",
                "done",
                "thread_updated"
            ]
        );
        let runs = hub.runs(Some("sess-1"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "stopped");
        assert_eq!(runs[0].tokens, Some(100));
        assert!(runs[0].cost_usd.is_some());
    }

    #[test]
    fn desktop_run_is_mirrored_but_not_persisted() {
        let hub = hub();
        hub.on_chat_payload("desk-1", &agent(AgentEvent::Token { delta: "hi".into() }));
        assert!(hub.is_running("desk-1"));
        hub.on_chat_payload(
            "desk-1",
            &agent(AgentEvent::Done {
                total_tokens: None,
                run_id: None,
            }),
        );
        assert!(!hub.is_running("desk-1"));
        assert!(hub
            .store
            .load_session_messages("desk-1")
            .unwrap()
            .is_empty());
        assert_eq!(hub.runs(None)[0].status, "done");
    }

    #[test]
    fn multi_agent_turn_finishes_after_every_done() {
        let hub = hub();
        hub.begin_run("s", None, None, None);
        hub.on_chat_payload(
            "s",
            &json!({ "type": "orchestrator_route", "agents": ["a", "b"], "reason": "arena" }),
        );
        hub.on_chat_payload(
            "s",
            &agent(AgentEvent::Done {
                total_tokens: Some(1),
                run_id: None,
            }),
        );
        assert!(hub.is_running("s"));
        hub.on_chat_payload(
            "s",
            &agent(AgentEvent::Done {
                total_tokens: Some(2),
                run_id: None,
            }),
        );
        assert!(!hub.is_running("s"));
        assert_eq!(hub.runs(None)[0].tokens, Some(3));
    }

    #[test]
    fn approvals_are_tracked_and_resolved() {
        let hub = hub();
        let mut rx = hub.subscribe();
        hub.begin_run("s", None, None, None);
        hub.on_chat_payload(
            "s",
            &agent(AgentEvent::ApprovalRequest {
                run_id: "gw-run-9".into(),
                tool: Some("bash".into()),
                preview: Some("rm -rf build".into()),
                choices: vec!["once".into(), "deny".into()],
                request: json!({ "command": "rm -rf build" }),
            }),
        );
        let pend = hub.pending_approvals();
        assert_eq!(pend.len(), 1);
        assert_eq!(pend[0].id, "gw-run-9");
        assert_eq!(pend[0].risk, "exec");
        assert_eq!(pend[0].args_preview, "rm -rf build");
        assert_eq!(hub.pending_approvals_for("s"), 1);
        let v = hub.mark_approval_resolved("gw-run-9", "approve").unwrap();
        assert!(v.resolved);
        assert!(hub.pending_approvals().is_empty());
        hub.on_chat_payload(
            "s",
            &agent(AgentEvent::ApprovalResolved {
                run_id: "gw-run-9".into(),
                choice: "once".into(),
            }),
        );
        let mut types = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let v = serde_json::to_value(&ev).unwrap();
            types.push(v["type"].as_str().unwrap().to_string());
            if v["type"] == "approval_resolved" {
                assert_eq!(v["decision"], "approve");
            }
        }
        assert_eq!(
            types,
            vec!["approval_request", "approval_resolved", "approval_resolved"]
        );
    }

    #[test]
    fn hook_block_error_ends_a_mobile_run_with_a_system_message() {
        let hub = hub();
        let (run_id, _) = hub.begin_run("s", None, None, None);
        hub.on_chat_payload("s", &json!({ "type": "error", "message": "hook: nope" }));
        assert!(!hub.is_running("s"));
        let msgs = hub.store.load_session_messages("s").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[0].content.contains("hook: nope"));
        assert_eq!(hub.run(&run_id).unwrap().view().status, "error");
    }

    #[test]
    fn risk_hints() {
        assert_eq!(risk_hint("bash", "ls"), "exec");
        assert_eq!(risk_hint("web_fetch", ""), "network");
        assert_eq!(risk_hint("read_file", "https://x"), "network");
        assert_eq!(risk_hint("write_file", ""), "write");
        assert_eq!(risk_hint("read_file", ""), "read");
        assert_eq!(risk_hint("mystery", ""), "unknown");
    }

    #[test]
    fn preview_truncates_on_char_boundary() {
        assert_eq!(preview("héllo wörld", 5), "héllo…");
        assert_eq!(preview("short", 10), "short");
    }
}
