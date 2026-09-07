//! HTTP handlers for the mobile server. JSON in / JSON out, except the static
//! SPA fallback (in [`super::router`]). Long-running work (chat, ultimate) is
//! spawned onto background tasks that stream progress over the `/ws` broadcast
//! channel; the POST endpoints return immediately with a `run_id` the client
//! correlates against.

use std::path::PathBuf;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use serde_json::json;

use crate::agents::adapter::{AgentEvent, ChatRequest, ChatTurn};
use crate::app_state::AppState;
use crate::gateway::client::GatewayClient;
use crate::orchestrator::ultimate::{self, UltimateConfig};

use super::state::{MobileEvent, MobileState, PendingApproval};

/// The agent id used when the client doesn't pin a model. Matches the AG-UI
/// server's choice — the gateway is Cortex's default orchestrator.
const DEFAULT_AGENT: &str = "gateway-remote";

// ───────────────────────────────────────────────────────────────────────────
// GET /api/health
// ───────────────────────────────────────────────────────────────────────────

pub async fn health() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// GET /api/projects
// ───────────────────────────────────────────────────────────────────────────

/// Project roster — reuses the same discovery the `list_projects` Tauri command
/// drives (vault root sourced from the app config).
pub async fn projects(State(state): State<MobileState>) -> impl IntoResponse {
    let vault_root = state.app.config.read().obsidian_vault.clone();
    Json(crate::projects::discover_projects(vault_root))
}

// ───────────────────────────────────────────────────────────────────────────
// GET /api/models
// ───────────────────────────────────────────────────────────────────────────

/// The connected-model roster — the same `discover_models` the ultimate agent
/// fans out across.
pub async fn models(State(state): State<MobileState>) -> impl IntoResponse {
    Json(ultimate::discover_models(&state.app.registry).await)
}

// ───────────────────────────────────────────────────────────────────────────
// OpenAI-compatible /v1 — lets the desktop app reach this server's real CLI
// sessions (claude-cli, codex-cli) + Ollama as a Model Fabric endpoint, so the
// full app runs Claude/Codex on THIS host over Tailscale (no local install, no
// gateway).
// ───────────────────────────────────────────────────────────────────────────

/// Resolve the adapter id for a requested model. Same routing as `/api/chat`:
/// exact adapter id → `ollama:` prefix → claude/gpt family → local CLI (when
/// present) → default gateway.
fn resolve_adapter_id(state: &MobileState, model: Option<&str>) -> String {
    let avail = |id: &str| {
        state.app.registry.read().get(id).map_or(false, |a| a.descriptor().available)
    };
    let is_claude = |m: &str| ["claude", "opus", "sonnet", "haiku"].iter().any(|p| m.starts_with(p));
    let is_gpt = |m: &str| {
        m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3")
            || m.starts_with("o4") || m.contains("codex")
    };
    match model {
        Some(m) if state.app.registry.read().get(m).is_some() => m.to_string(),
        Some(m) if m.starts_with("ollama:") || m.starts_with("ollama/") => "ollama".to_string(),
        Some(m) if is_claude(m) && avail("claude-cli") => "claude-cli".to_string(),
        Some(m) if is_gpt(m) && avail("codex-cli") => "codex-cli".to_string(),
        _ => DEFAULT_AGENT.to_string(),
    }
}

// GET /v1/models — OpenAI list format.
pub async fn v1_models(State(state): State<MobileState>) -> impl IntoResponse {
    let data: Vec<_> = ultimate::discover_models(&state.app.registry)
        .await
        .into_iter()
        .map(|id| json!({ "id": id, "object": "model", "owned_by": "cortex" }))
        .collect();
    Json(json!({ "object": "list", "data": data }))
}

// POST /v1/chat/completions — always streams SSE (OpenAI chat.completion.chunk).
pub async fn v1_chat_completions(
    State(state): State<MobileState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::sse::{Event, Sse};

    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let msgs = body.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut history: Vec<ChatTurn> = msgs
        .iter()
        .map(|m| ChatTurn {
            role: m.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string(),
            content: m.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            agent: None,
        })
        .collect();
    // The trailing user turn is the current message; the rest is history.
    let message = if history.last().map(|t| t.role.as_str()) == Some("user") {
        history.pop().map(|t| t.content).unwrap_or_default()
    } else {
        history.last().map(|t| t.content.clone()).unwrap_or_default()
    };

    let adapter_id = resolve_adapter_id(&state, Some(model.as_str()).filter(|m| !m.is_empty()));
    let agent = state
        .app
        .registry
        .read()
        .get(&adapter_id)
        .or_else(|| state.app.registry.read().get(DEFAULT_AGENT));
    let Some(agent) = agent else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "no agent" }))).into_response();
    };
    let project_root = state.app.config.read().default_project_root.clone();
    let chat_req = ChatRequest {
        session_id: format!("v1:{}", uuid::Uuid::new_v4()),
        message,
        project_root,
        history,
        // Don't pass the adapter id itself as a model slug.
        model: (!model.is_empty() && model != adapter_id).then(|| model.clone()),
        reasoning_effort: None,
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    tokio::spawn(async move {
        let _ = agent.run(chat_req, tx).await;
    });

    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let stream = futures::stream::unfold((rx, id, model, false), |(mut rx, id, model, done)| async move {
        if done {
            return None;
        }
        loop {
            match rx.recv().await {
                Some(AgentEvent::Token { delta }) => {
                    let e = Event::default().data(
                        json!({"id":id,"object":"chat.completion.chunk","model":model,
                               "choices":[{"index":0,"delta":{"content":delta}}]})
                        .to_string(),
                    );
                    return Some((Ok::<_, std::convert::Infallible>(e), (rx, id, model, false)));
                }
                Some(AgentEvent::Error { message }) => {
                    let e = Event::default().data(
                        json!({"id":id,"object":"chat.completion.chunk","model":model,
                               "choices":[{"index":0,"delta":{"content":format!("[error] {message}")},"finish_reason":"stop"}]})
                        .to_string(),
                    );
                    return Some((Ok(e), (rx, id, model, false)));
                }
                Some(AgentEvent::Done { .. }) | None => {
                    return Some((Ok(Event::default().data("[DONE]")), (rx, id, model, true)));
                }
                Some(_) => continue, // skip Started/Reasoning/ToolCall/etc.
            }
        }
    });
    Sse::new(stream).into_response()
}

// ───────────────────────────────────────────────────────────────────────────
// POST /api/chat
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatBody {
    #[serde(default)]
    pub session_id: Option<String>,
    pub message: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
}

/// Start a chat run. Picks the requested model's adapter (else the default
/// gateway adapter), spawns `adapter.run(...)`, and bridges each [`AgentEvent`]
/// into a [`MobileEvent`] on the WS broadcast channel. Returns the `run_id` and
/// `session_id` immediately so the client can correlate the stream.
pub async fn chat(
    State(state): State<MobileState>,
    Json(body): Json<ChatBody>,
) -> impl IntoResponse {
    if body.message.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "empty message" }))).into_response();
    }

    let run_id = format!("chat:{}", uuid::Uuid::new_v4());
    let session_id = body
        .session_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("mobile:{}", uuid::Uuid::new_v4()));

    // Resolve the ADAPTER for the requested model. A model id may be an adapter
    // id directly (e.g. gateway agents), OR a provider-prefixed model id like
    // "ollama:llama3.2:3b" whose adapter is "ollama" (the adapter strips the
    // prefix to choose the model). `body.model` is still passed through as the
    // model so the adapter selects the right one. Falls back to the gateway.
    // Route roster models (e.g. "claude-sonnet-4-6", "gpt-5.5") to their real
    // local CLI adapter when it's registered + available, so picking a model in
    // the roster runs the actual Claude/Codex session instead of falling through
    // to a (maybe-unconfigured) gateway. Only kicks in when the CLI is present.
    let avail = |id: &str| {
        state
            .app
            .registry
            .read()
            .get(id)
            .map_or(false, |a| a.descriptor().available)
    };
    let is_claude = |m: &str| {
        ["claude", "opus", "sonnet", "haiku"].iter().any(|p| m.starts_with(p))
    };
    let is_gpt = |m: &str| {
        m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3")
            || m.starts_with("o4") || m.contains("codex")
    };
    let adapter_id = match body.model.as_deref() {
        Some(m) if state.app.registry.read().get(m).is_some() => m.to_string(),
        Some(m) if m.starts_with("ollama:") || m.starts_with("ollama/") => "ollama".to_string(),
        Some(m) if is_claude(m) && avail("claude-cli") => "claude-cli".to_string(),
        Some(m) if is_gpt(m) && avail("codex-cli") => "codex-cli".to_string(),
        _ => DEFAULT_AGENT.to_string(),
    };
    let agent = state
        .app
        .registry
        .read()
        .get(&adapter_id)
        .or_else(|| state.app.registry.read().get(DEFAULT_AGENT));

    let Some(agent) = agent else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": format!("no agent '{adapter_id}' (and no default) registered") })),
        )
            .into_response();
    };

    // Project root: explicit override, else the app's default.
    let project_root: Option<PathBuf> = body
        .project_root
        .map(PathBuf::from)
        .or_else(|| state.app.config.read().default_project_root.clone());

    // If the requested "model" is really the adapter id itself (the client picked
    // the agent, e.g. `claude-cli`, not a concrete model slug), don't forward it
    // as the model — a CLI adapter would pass it straight to `--model` and the
    // tool would reject it. Passing None lets the adapter use its own default.
    let model = body.model.filter(|m| *m != adapter_id);
    let chat_req = ChatRequest {
        session_id: session_id.clone(),
        message: body.message,
        project_root,
        history: Vec::<ChatTurn>::new(),
        model,
        reasoning_effort: None,
    };

    state.publish(MobileEvent::ChatStarted {
        run_id: run_id.clone(),
        session_id: session_id.clone(),
    });

    // Spawn the run; bridge AgentEvents → MobileEvents over the broadcast.
    let pub_state = state.clone();
    let run_id_task = run_id.clone();
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
        let run_id_inner = run_id_task.clone();
        let forwarder = {
            let pub_state = pub_state.clone();
            tokio::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    bridge_agent_event(&pub_state, &run_id_inner, ev);
                }
            })
        };
        if let Err(e) = agent.run(chat_req, tx).await {
            pub_state.publish(MobileEvent::ChatError {
                run_id: run_id_task.clone(),
                message: e.to_string(),
            });
        }
        // `agent.run` returning drops `tx`; the forwarder drains then exits.
        let _ = forwarder.await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "run_id": run_id, "session_id": session_id })),
    )
        .into_response()
}

/// Translate one [`AgentEvent`] into a [`MobileEvent`] and publish it. Approval
/// requests are additionally recorded in the pending-approval store so
/// `GET /api/approvals` can list them.
fn bridge_agent_event(state: &MobileState, run_id: &str, ev: AgentEvent) {
    let run_id = run_id.to_string();
    // This is an independent event stream from the desktop chat path (its
    // own `agent.run()` call, not a tap into `commands::chat`'s pipeline —
    // see the caller), so it needs its own redaction pass rather than
    // inheriting `chat.rs`'s. Same rationale: ToolCall/ApprovalRequest carry
    // free-form tool-supplied text that can embed a live credential, and
    // this is displayed on the mobile client AND persisted in the
    // in-memory `PendingApproval` store served by `GET /api/approvals`.
    let ev = ev.redacted_for_display();
    match ev {
        AgentEvent::Started { .. } => {}
        AgentEvent::Token { delta } => {
            state.publish(MobileEvent::ChatToken { run_id, delta });
        }
        AgentEvent::Reasoning { text } => {
            state.publish(MobileEvent::ChatReasoning { run_id, text });
        }
        AgentEvent::ToolCall { name, preview, .. } => {
            state.publish(MobileEvent::ChatToolCall { run_id, name, preview });
        }
        AgentEvent::ToolResult { name, ok, summary, .. } => {
            state.publish(MobileEvent::ChatToolResult { run_id, name, ok, summary });
        }
        AgentEvent::FileEdit { path, lines_changed } => {
            state.publish(MobileEvent::ChatFileEdit {
                run_id,
                path: path.to_string_lossy().into_owned(),
                lines_changed,
            });
        }
        AgentEvent::ApprovalRequest {
            run_id: req_run_id,
            tool,
            preview,
            choices,
            request,
        } => {
            // The approval id mirrors the run id the agent emitted (one open
            // approval per run at a time). Record it so `GET /api/approvals`
            // and `POST /api/approvals/{id}` can act on it.
            let id = req_run_id.clone();
            state.approvals.lock().push(PendingApproval {
                id,
                run_id: req_run_id,
                tool: tool.clone(),
                preview: preview.clone(),
                choices: choices.clone(),
                request,
            });
            state.publish(MobileEvent::ChatApproval { run_id, tool, preview, choices });
        }
        AgentEvent::ApprovalResolved { run_id: rr, choice } => {
            state.approvals.lock().retain(|a| a.id != rr);
            state.publish(MobileEvent::ChatApprovalResolved { run_id, choice });
        }
        AgentEvent::Error { message } => {
            state.publish(MobileEvent::ChatError { run_id, message });
        }
        AgentEvent::Done { total_tokens, .. } => {
            state.publish(MobileEvent::ChatDone { run_id, total_tokens });
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// POST /api/ultimate
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UltimateBody {
    pub goal: String,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub fan_out: Option<usize>,
    #[serde(default)]
    pub lead_model: Option<String>,
}

/// Run the ultimate multi-model orchestrator. Each [`ultimate::UltEvent`] is
/// streamed over `/ws` (wrapped in [`MobileEvent::Ultimate`]); the final
/// [`ultimate::UltimateResult`] is BOTH returned in the JSON response and
/// published as [`MobileEvent::UltimateDone`]. Because the whole run is awaited
/// (it's a one-shot request/response from the composer) this endpoint blocks
/// until completion — matching the `ultimate_chat_run` Tauri command.
pub async fn ultimate(
    State(state): State<MobileState>,
    Json(body): Json<UltimateBody>,
) -> impl IntoResponse {
    if body.goal.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "empty goal" }))).into_response();
    }

    let run_id = format!("ultimate:{}", uuid::Uuid::new_v4());
    let cfg = UltimateConfig {
        goal: body.goal,
        project_root: body.project_root,
        fan_out: body.fan_out.unwrap_or(3),
        lead_model: body.lead_model,
    };

    let registry = state.app.registry.clone();
    let store = state.store.clone();

    // Stream each engine event verbatim over the WS broadcast. The closure must
    // be Send + Sync (the engine fans subtasks across tasks); cloning the cheap
    // MobileState handle into it satisfies that.
    let emit_state = state.clone();
    let emit_run_id = run_id.clone();
    let result = ultimate::run_ultimate(registry, store, cfg, move |ev| {
        let event = serde_json::to_value(&ev).unwrap_or(serde_json::Value::Null);
        emit_state.publish(MobileEvent::Ultimate {
            run_id: emit_run_id.clone(),
            event,
        });
    })
    .await;

    match result {
        Ok(res) => {
            let result_json = serde_json::to_value(&res).unwrap_or(serde_json::Value::Null);
            state.publish(MobileEvent::UltimateDone {
                run_id: run_id.clone(),
                result: result_json,
            });
            (StatusCode::OK, Json(json!({ "run_id": run_id, "result": res }))).into_response()
        }
        Err(e) => {
            state.publish(MobileEvent::UltimateError {
                run_id: run_id.clone(),
                message: e.clone(),
            });
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "run_id": run_id, "error": e })),
            )
                .into_response()
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// GET /api/sessions  +  GET /api/sessions/{id}/messages
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Recent chat sessions, newest first — the "Recent chats" list the mobile app
/// shows so a user can reopen + resume a past conversation. Reuses the
/// `TracingStore::recent_chat_sessions` reader (distinct `session_id` from the
/// `messages` table with a derived title/preview). `limit` defaults to 50 and
/// is clamped to a sane ceiling so a hostile query can't fan out unboundedly.
pub async fn sessions(
    State(state): State<MobileState>,
    axum::extract::Query(q): axum::extract::Query<SessionsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    match state.store.recent_chat_sessions(limit) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Full message history for one session, oldest first — loaded into the Chat
/// view when a user taps a recent session. Reuses the same
/// `TracingStore::load_session_messages` reader the desktop `load_session_messages`
/// Tauri command drives, so mobile + desktop see identical history.
pub async fn session_messages(
    State(state): State<MobileState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.load_session_messages(&id) {
        Ok(msgs) => Json(msgs).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// POST /api/sessions/{id}/export — Save a session to the Obsidian vault
// ───────────────────────────────────────────────────────────────────────────

/// Export a session (incl. imported Claude.ai/ChatGPT history) to the Obsidian
/// vault as Markdown — the mobile twin of the desktop "Save to Brain" action.
/// Reuses the exact same `export_session_markdown` core, so mobile + desktop
/// produce byte-identical notes.
pub async fn export_session(
    State(state): State<MobileState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let vault = state.app.config.read().obsidian_vault.clone();
    match crate::commands::sessions::export_session_markdown(&state.store, vault, &id) {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Semantic search over chat history — GET /api/search + POST /api/search/reindex
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Build/refresh the chat semantic index — embeds any not-yet-embedded messages
/// (incl. imported Claude/ChatGPT history) via the local Ollama embedder.
pub async fn search_reindex(State(state): State<MobileState>) -> impl IntoResponse {
    let (base, vault) = {
        let c = state.app.config.read();
        (c.ollama_base_url.clone(), c.obsidian_vault.clone())
    };
    // Refresh both indexes — chat messages AND vault/memory notes.
    match crate::commands::chat_semantic::reindex_all(&state.store, &base, vault, None).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response(),
    }
}

/// Unified RAG — "chat with your brain" over notes + chat history with citations.
pub async fn brain(
    State(state): State<MobileState>,
    Json(body): Json<BrainBody>,
) -> impl IntoResponse {
    let (ollama_base, vault, cfg_model) = {
        let cfg = state.app.config.read();
        (cfg.ollama_base_url.clone(), cfg.obsidian_vault.clone(), cfg.ollama_model.clone())
    };
    let chat_model = crate::commands::brain_rag::resolve_chat_model(body.model, cfg_model);
    let pr = body.project_root.map(std::path::PathBuf::from);
    match crate::commands::brain_rag::brain_answer(
        &state.store,
        &ollama_base,
        &chat_model,
        vault,
        pr,
        &body.question,
        body.limit.unwrap_or(8),
    )
    .await
    {
        Ok(ans) => (StatusCode::OK, Json(ans)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct BrainBody {
    pub question: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
}

/// Semantic search across chat history by meaning (cosine over the embedded
/// index), newest-first ties broken by score.
pub async fn search_chat(
    State(state): State<MobileState>,
    axum::extract::Query(q): axum::extract::Query<SearchQuery>,
) -> impl IntoResponse {
    let base = state.app.config.read().ollama_base_url.clone();
    let model = crate::memory::embed::embed_model();
    match crate::commands::chat_semantic::search(
        &state.store,
        &base,
        &model,
        &q.q,
        q.limit.unwrap_or(10),
        false, // chat-history search stays chats-only (results link to sessions)
    )
    .await
    {
        Ok(hits) => (StatusCode::OK, Json(hits)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// GET /api/approvals  +  POST /api/approvals/{id}
// ───────────────────────────────────────────────────────────────────────────

/// List the approvals currently awaiting a decision. Populated as chat runs
/// surface `ApprovalRequest` events and drained when resolved (via
/// `POST /api/approvals/{id}`) or when the run reports an `ApprovalResolved`.
///
/// Live re-injection: the desktop app resolves approvals by POSTing the
/// decision back to the Cortex Gateway keyed by the run id
/// (`chat.rs::approve_run` → `GatewayClient::approve_run`); the gateway holds
/// the paused run and resumes it. The mobile bridge uses the **same**
/// mechanism — `resolve_approval` POSTs to the gateway with the stored
/// `run_id`. Because the gateway-backed `gateway-remote` adapter is the only
/// adapter that emits an `ApprovalRequest` (and the `run_id` it carries is the
/// gateway's run id), a decision from the mobile API resumes the in-flight run
/// exactly as a desktop decision would. Adapters that self-approve via policy
/// never surface a prompt here, so there is nothing to re-inject for them.
pub async fn list_approvals(State(state): State<MobileState>) -> impl IntoResponse {
    let pending = state.approvals.lock().clone();
    Json(pending)
}

#[derive(Debug, Deserialize)]
pub struct ResolveApprovalBody {
    pub approve: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Resolve a pending approval by id and **re-inject** the decision into the
/// in-flight run so it actually resumes.
///
/// Mirrors `chat.rs::approve_run`: builds a [`GatewayClient`] from the shared
/// app config + the gateway API key, and POSTs the decision to the gateway
/// keyed by the stored `run_id`. The gateway is what holds the paused
/// `gateway-remote` run, so this POST is the live re-injection — the resumed
/// run streams its continuation back over `/ws` (the bridged `AgentEvent`s),
/// and the gateway's own `ApprovalResolved` will also surface. We additionally
/// publish a `ChatApprovalResolved` immediately so every connected client
/// drops the pending prompt without waiting for the SSE round-trip.
///
/// Returns 404 when there is no such pending decision, and 502 when the gateway
/// rejects the re-injection (the entry is restored so a retry can re-resolve).
pub async fn resolve_approval(
    State(state): State<MobileState>,
    Path(id): Path<String>,
    Json(body): Json<ResolveApprovalBody>,
) -> impl IntoResponse {
    let removed = {
        let mut pend = state.approvals.lock();
        let before = pend.len();
        let entry = pend.iter().find(|a| a.id == id).cloned();
        pend.retain(|a| a.id != id);
        if pend.len() < before {
            entry
        } else {
            None
        }
    };

    let Some(entry) = removed else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": format!("no pending approval '{id}'") })),
        )
            .into_response();
    };

    // Same choice vocabulary the gateway/desktop path uses.
    let choice = if body.approve { "approve" } else { "deny" };

    // Re-inject into the live run: POST the decision to the gateway keyed by the
    // run id, exactly as the desktop `approve_run` command does. This is what
    // resumes the paused run.
    let cfg = state.app.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    if let Err(e) = client
        .approve_run(&entry.run_id, choice, None, None, body.reason.clone())
        .await
    {
        // Re-injection failed — restore the pending entry so the client can
        // retry, and report the failure rather than silently dropping it.
        state.approvals.lock().push(entry);
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "ok": false,
                "id": id,
                "error": format!("failed to re-inject approval decision into the run: {e}"),
            })),
        )
            .into_response();
    }

    // Tell every connected client the prompt is resolved now (the gateway will
    // also emit its own ApprovalResolved over the resumed run's stream).
    state.publish(MobileEvent::ChatApprovalResolved {
        run_id: entry.run_id.clone(),
        choice: choice.to_string(),
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "id": id,
            "run_id": entry.run_id,
            "approve": body.approve,
            "reason": body.reason,
            "note": "decision re-injected into the in-flight run via the gateway"
        })),
    )
        .into_response()
}

// ───────────────────────────────────────────────────────────────────────────
// POST /api/import/file  +  POST /api/import/pull
// ───────────────────────────────────────────────────────────────────────────

/// Accept a chat-history export as raw JSON *content* in the body (so the phone
/// can paste/upload without filesystem access) and import it. `format` defaults
/// to auto-detect. Body size is capped by [`crate::chat_import::MAX_IMPORT_BYTES`].
#[derive(Debug, Deserialize)]
pub struct ImportFileBody {
    /// The raw export JSON (e.g. the contents of `conversations.json`).
    pub content: String,
    /// `"auto"` (default) | `"claude"` | `"chatgpt"` | `"generic"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Import a pasted/uploaded export. On success the JSON result carries the new
/// `session_ids`; the client refreshes Recent chats from `GET /api/sessions`.
pub async fn import_file(
    State(state): State<MobileState>,
    Json(body): Json<ImportFileBody>,
) -> impl IntoResponse {
    let fmt = body
        .format
        .as_deref()
        .and_then(crate::chat_import::format_from_str);
    match crate::chat_import::import_from_str(&body.content, fmt, &state.store).await {
        Ok(res) => (StatusCode::OK, Json(json!(res))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response(),
    }
}

/// EXPERIMENTAL: pull chat history live via a session token and import it.
/// `provider` is `"claude"` or `"chatgpt"`. The token is never logged.
#[derive(Debug, Deserialize)]
pub struct ImportPullBody {
    pub provider: String,
    pub token: String,
}

pub async fn import_pull(
    State(state): State<MobileState>,
    Json(body): Json<ImportPullBody>,
) -> impl IntoResponse {
    match crate::chat_import::import_from_pull(&body.provider, &body.token, &state.store).await {
        Ok(res) => (StatusCode::OK, Json(json!(res))).into_response(),
        // 502: the failure is almost always an upstream/unofficial-API problem.
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e }))).into_response(),
    }
}
