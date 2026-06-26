//! `axum` HTTP/SSE server that exposes Cortex as an AG-UI Protocol server.
//!
//! # Endpoints
//!
//! - `GET  /agui/health` — JSON `{ ok: true, version: "0.1" }`.
//! - `POST /agui/run`    — AG-UI run dispatch. Accepts a `RunAgentInput`
//!   body and responds with an SSE stream of AG-UI events. Dispatches the
//!   last user message of `messages[]` into the gateway remote adapter
//!   (`gateway-remote`) via the registry and streams its `AgentEvent`s back
//!   through the [`translator`] into AG-UI wire frames.
//!
//! Off by default. The Tauri command `start_agui_server` (see
//! `commands/agui.rs`) spawns this server on demand; `stop_agui_server`
//! flips an `AtomicBool` that `axum`'s `with_graceful_shutdown` watches.
//!
//! Loopback-only. No auth headers. Don't expose publicly.
//!
//! # Wiring
//!
//! Referenced as `pub mod server;` from `agui/mod.rs`. The Tauri command
//! layer (`commands/agui.rs`) is what actually calls [`spawn`].

use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use axum::{
    extract::State,
    http::HeaderValue,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures::stream::{self, Stream};
use serde_json::json;
use tokio::sync::{mpsc, Notify};
use tower_http::cors::{Any, CorsLayer};
use ulid::Ulid;

use super::{
    translator::{translate, TranslatorState},
    AgUiEvent, ErrorEvent, RunAgentInput, RunFinished, DEFAULT_BIND, PROTOCOL_VERSION,
};
use crate::agents::adapter::{ChatRequest, ChatTurn};
use crate::app_state::AppState;

/// Shared state given to every axum handler. Holds the [`AppState`] so the
/// `/agui/run` handler can reach the agent registry.
#[derive(Clone)]
struct AguiServerState {
    app: AppState,
}

/// Handle returned to the caller of [`spawn`] so the server can be stopped.
#[derive(Clone)]
pub struct AguiServerHandle {
    addr: SocketAddr,
    shutdown: Arc<Notify>,
    running: Arc<AtomicBool>,
}

impl AguiServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Signal the server task to shut down gracefully. Idempotent.
    pub fn stop(&self) {
        if self.running.swap(false, Ordering::SeqCst) {
            self.shutdown.notify_waiters();
        }
    }
}

/// Spawn the AG-UI server. `bind` defaults to [`DEFAULT_BIND`].
pub async fn spawn(
    bind: Option<SocketAddr>,
    app: AppState,
) -> anyhow::Result<AguiServerHandle> {
    let addr: SocketAddr = match bind {
        Some(a) => a,
        None => DEFAULT_BIND
            .parse()
            .expect("DEFAULT_BIND is a valid SocketAddr"),
    };

    let shutdown = Arc::new(Notify::new());
    let running = Arc::new(AtomicBool::new(true));

    let state = AguiServerState { app };
    let router = router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr().unwrap_or(addr);

    let shutdown_clone = shutdown.clone();
    let running_clone = running.clone();
    tokio::spawn(async move {
        let server = axum::serve(listener, router.into_make_service())
            .with_graceful_shutdown(async move { shutdown_clone.notified().await });
        if let Err(e) = server.await {
            tracing::error!(error = %e, "agui server exited with error");
        }
        running_clone.store(false, Ordering::SeqCst);
        tracing::info!("agui server stopped");
    });

    tracing::info!(addr = %bound_addr, "agui server listening");
    Ok(AguiServerHandle {
        addr: bound_addr,
        shutdown,
        running,
    })
}

fn router(state: AguiServerState) -> Router {
    // This server dispatches agent runs with no auth and listens on
    // loopback. A wildcard `Access-Control-Allow-Origin: *` would let any
    // website the user visits issue cross-origin `POST /agui/run` requests
    // (drive-by CSRF). Restrict CORS to known local origins (the Tauri app
    // and the dev server) so browsers reject everyone else. Note: this only
    // protects browser-based callers — the loopback-only bind plus the
    // module's "don't expose publicly" contract remain the primary defense.
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins())
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/agui/health", get(health))
        .route("/agui/run", post(run))
        .with_state(state)
        .layer(cors)
}

/// Origins permitted to make cross-origin requests to this server. The Tauri
/// webview runs under the custom `tauri://localhost` scheme (and
/// `https://tauri.localhost` on Windows/Android); the Vite dev server runs on
/// loopback HTTP. Everything else — i.e. any real website — is rejected by the
/// browser's CORS preflight, blocking drive-by CSRF.
fn allowed_origins() -> Vec<HeaderValue> {
    [
        "tauri://localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
        "http://127.0.0.1:1420",
    ]
    .iter()
    .filter_map(|o| HeaderValue::from_str(o).ok())
    .collect()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "version": PROTOCOL_VERSION,
    }))
}

/// Flatten an AG-UI message `content` field into plain text.
///
/// AG-UI allows `content` to be either a plain string or an array of content
/// parts (e.g. `[{ "type": "text", "text": "..." }]`, as used for multimodal
/// or tool messages). Treating only the string form as valid silently dropped
/// every structured message — losing history and sometimes the prompt itself.
/// Here we handle both: strings pass through, arrays have their textual parts
/// concatenated, and anything else degrades to an empty string (still skipped
/// by the caller, preserving prior empty-content behavior).
fn extract_content(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                // A bare string element, or an object carrying a "text"
                // (or "content") field — the common AG-UI text-part shapes.
                let text = part
                    .as_str()
                    .or_else(|| part.get("text").and_then(|v| v.as_str()))
                    .or_else(|| part.get("content").and_then(|v| v.as_str()));
                if let Some(t) = text {
                    if !t.is_empty() {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(t);
                    }
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Extract `{ role, content }` pairs from AG-UI's messages array, mapping
/// AG-UI's free-form role values onto Cortex's "user" / "assistant" /
/// "system" trichotomy. Returns `(history, last_user_prompt)`.
///
/// The LAST user message becomes the prompt fed to the agent's `run()`.
/// Everything else becomes Cortex's `history`. If no user message is
/// present we fall back to the last message of any role.
fn split_messages(messages: &[serde_json::Value]) -> (Vec<ChatTurn>, String) {
    let mut turns: Vec<(String, String)> = messages
        .iter()
        .filter_map(|m| {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = extract_content(m.get("content"));
            if content.is_empty() {
                None
            } else {
                let normalized = match role {
                    "system" | "assistant" | "user" => role.to_string(),
                    "tool" => "assistant".to_string(),
                    other => other.to_string(),
                };
                Some((normalized, content))
            }
        })
        .collect();

    // Find the last "user" turn from the back. If none, take the very last.
    let prompt_idx = turns
        .iter()
        .rposition(|(r, _)| r == "user")
        .or_else(|| turns.len().checked_sub(1));

    let prompt = match prompt_idx {
        Some(i) => {
            let (_, c) = turns.remove(i);
            c
        }
        None => String::new(),
    };

    let history = turns
        .into_iter()
        .map(|(role, content)| ChatTurn { role, content, agent: None })
        .collect();

    (history, prompt)
}

/// `POST /agui/run` — real gateway dispatch over SSE.
async fn run(
    State(state): State<AguiServerState>,
    Json(input): Json<RunAgentInput>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::info!(
        thread_id = %input.thread_id,
        run_id = %input.run_id,
        msgs = input.messages.len(),
        tools = input.tools.len(),
        "agui /run dispatching"
    );

    // Channel from the run-task → SSE response stream. Generous buffer
    // because a single token can fan out into 2 AG-UI events and the agent
    // produces deltas at LLM speed (10s–100s/sec).
    //
    // We forward into it with `try_send` (see below) rather than `send().await`
    // so a slow SSE client that stops draining the response can never block the
    // forwarder — and, transitively, pin the agent task — once the buffer
    // fills. A backed-up client instead trips the buffer-full case and we tear
    // the run down. The generous depth absorbs normal bursty fan-out.
    let (frame_tx, frame_rx) = mpsc::channel::<Result<Event, Infallible>>(256);

    let agent = state.app.registry.read().get("gateway-remote");
    let project_root: Option<PathBuf> = state
        .app
        .config
        .read()
        .default_project_root
        .clone();

    let thread_id = input.thread_id.clone();
    let run_id = input.run_id.clone();

    match agent {
        Some(agent) => {
            let (history, prompt) = split_messages(&input.messages);
            let chat_req = ChatRequest {
                session_id: format!("agui:{}", input.thread_id),
                message: prompt,
                project_root,
                history,
                model: None,
                reasoning_effort: None,
            };

            let mut translator_state =
                TranslatorState::new(Some(thread_id), Some(run_id.clone()));

            // Spawn the agent run on a dedicated task. It pushes
            // `AgentEvent`s into `agent_rx`; we translate + forward.
            let (agent_tx, agent_rx) = mpsc::channel(128);
            let agent_clone = agent.clone();
            tokio::spawn(async move {
                if let Err(e) = agent_clone.run(chat_req, agent_tx).await {
                    tracing::error!(error = %e, "agui: agent.run() returned error");
                }
            });

            // Forwarder: read agent events, translate, push SSE frames.
            tokio::spawn(async move {
                let mut agent_rx = agent_rx;
                // The translator already emits a RUN_FINISHED when it sees the
                // agent's `Done` event. Track whether one came through so we
                // don't emit a second, duplicate RUN_FINISHED below — the
                // unconditional one is only a fallback for streams that end
                // *without* a Done (dropped sender, network error, panic).
                let mut run_finished = false;
                while let Some(ev) = agent_rx.recv().await {
                    for agui_event in translate(&mut translator_state, &ev) {
                        if matches!(agui_event, AgUiEvent::RunFinished(_)) {
                            run_finished = true;
                        }
                        let frame = encode_frame(&agui_event);
                        // Non-blocking: a slow client that stops reading the SSE
                        // response must not stall this forwarder (and, via the
                        // upstream `agent_rx`, the agent task). On a full buffer
                        // or a closed receiver we abandon the run rather than
                        // park here indefinitely.
                        match frame_tx.try_send(Ok(frame)) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!(
                                    "agui: SSE buffer full; client too slow, dropping run"
                                );
                                return;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                return; // client disconnected
                            }
                        }
                    }
                }
                // Belt-and-braces: if the agent dropped its sender without
                // emitting a Done (network error, panic, etc.), still
                // close the AG-UI run cleanly. The translator's own state
                // is gone here; emit a bare RUN_FINISHED.
                // Terminal frames use `try_send` for the same reason as the
                // loop above: never park the forwarder on a wedged client. If
                // the buffer is full or closed these are simply dropped — the
                // run is ending anyway.
                if translator_state.current_message_id.is_some() {
                    // Open text — synthesize an END for safety.
                    let id = translator_state.current_message_id.take().unwrap();
                    let _ = frame_tx.try_send(Ok(encode_frame(
                        &AgUiEvent::TextMessageEnd(super::TextMessageEnd {
                            message_id: id,
                            timestamp: None,
                        }),
                    )));
                }
                if !run_finished {
                    let _ = frame_tx.try_send(Ok(encode_frame(
                        &AgUiEvent::RunFinished(RunFinished {
                            outcome: None,
                            timestamp: None,
                        }),
                    )));
                }
            });
        }
        None => {
            // No agent registered — emit a single ERROR + RUN_FINISHED so
            // the client still sees a well-formed stream.
            let err = AgUiEvent::Error(ErrorEvent {
                message: "no agent 'gateway-remote' registered in Cortex".into(),
                code: Some("agent_not_found".into()),
                timestamp: None,
            });
            let fin = AgUiEvent::RunFinished(RunFinished {
                outcome: Some(json!("agent_not_found")),
                timestamp: None,
            });
            let _ = frame_tx.try_send(Ok(encode_frame(&err)));
            let _ = frame_tx.try_send(Ok(encode_frame(&fin)));
        }
    }

    // Convert the mpsc receiver into a `Stream`. Using `stream::unfold` so
    // we don't pull in `tokio-stream` as a new top-level dependency.
    let stream = stream::unfold(frame_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Build an SSE `Event` for one AG-UI event. AG-UI puts the typed event
/// JSON in the `data:` field; no `event:` line is used.
fn encode_frame(ev: &AgUiEvent) -> Event {
    let body = serde_json::to_string(ev)
        .unwrap_or_else(|e| format!(r#"{{"type":"ERROR","message":"encode: {e}"}}"#));
    Event::default().data(body)
}

/// Unused, kept around so callers in tests can mint a stable id without
/// pulling `ulid` directly.
#[allow(dead_code)]
fn fresh_id() -> String {
    Ulid::new().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::Registry;
    use parking_lot::RwLock;

    fn empty_state() -> AppState {
        AppState {
            registry: Arc::new(RwLock::new(Registry::new())),
            config: Arc::new(RwLock::new(crate::app_state::Config::default())),
        }
    }

    #[tokio::test]
    async fn spawn_and_stop_roundtrip() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let handle = spawn(Some(addr), empty_state()).await.expect("spawn");
        assert!(handle.is_running());
        handle.stop();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!handle.is_running());
    }

    #[test]
    fn split_messages_picks_last_user_as_prompt() {
        let msgs = vec![
            json!({"role": "system", "content": "you are helpful"}),
            json!({"role": "user", "content": "first question"}),
            json!({"role": "assistant", "content": "first answer"}),
            json!({"role": "user", "content": "follow up"}),
        ];
        let (history, prompt) = split_messages(&msgs);
        assert_eq!(prompt, "follow up");
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].role, "user");
        assert_eq!(history[2].role, "assistant");
    }

    #[test]
    fn split_messages_handles_no_user_role() {
        let msgs = vec![json!({"role": "assistant", "content": "hi"})];
        let (history, prompt) = split_messages(&msgs);
        assert_eq!(prompt, "hi");
        assert!(history.is_empty());
    }

    #[test]
    fn encode_frame_produces_sse_data_line() {
        let ev = AgUiEvent::Error(ErrorEvent {
            message: "x".into(),
            code: None,
            timestamp: None,
        });
        let frame = encode_frame(&ev);
        // axum's Event impl doesn't expose its inner string directly, but
        // formatting via Debug still gives us enough to assert presence.
        let dbg = format!("{frame:?}");
        assert!(dbg.contains("ERROR"), "expected ERROR in {dbg}");
    }
}
