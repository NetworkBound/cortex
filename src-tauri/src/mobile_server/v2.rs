//! `/api/v2/*` — the mobile contract surface (see `mobile-contract.md`).
//!
//! Every route except `POST /pair` sits behind [`super::auth::v2_gate`]
//! (bearer for non-local peers). JSON in/out, `snake_case`, unix-ms times,
//! errors as `{ "error": { "code", "message" } }` via [`ApiError`].
//!
//! Chat goes through the desktop pipeline (`commands::chat::chat_send_with`)
//! so routing, hooks, tracing, failover and push all apply; streaming reaches
//! the phone over `/ws` via the [`super::events::V2Hub`] tap.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, patch, post},
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agents::ChatTurn;
use crate::app_state::AppState;
use crate::commands::chat::{self, ChatSendArgs};
use crate::commands::routines::{self, RoutineSpec};
use crate::gateway::client::GatewayClient;
use crate::observability::tracing_store::{StoredMessage, TracingStore};

use super::auth::{self, Access};
use super::events::{RunView, ThreadView, V2Hub};
use super::pairing;
use super::state::MobileState;
use super::threads;
use super::webpush;

/// Features this server implements — the client feature-detects on these.
pub const FEATURES: &[&str] = &[
    "threads",
    "replay",
    "routines",
    "git",
    "checkpoints",
    "reliability",
    "usage",
    "projects.add",
    "webpush",
    "push.web",
    "messages.after",
    "ws.since_ms",
];

/// Diff bodies are capped at 200 KB (contract).
const DIFF_CAP_BYTES: usize = 200 * 1024;
/// Text attachments inlined into the prompt are capped here.
const ATTACHMENT_TEXT_CAP: usize = 64 * 1024;
/// How many prior turns are replayed as history on a send.
const HISTORY_TURNS: usize = 40;

// ───────────────────────────────────────────────────────────────────────────
// Error envelope
// ───────────────────────────────────────────────────────────────────────────

/// `{ "error": { "code", "message" } }` with the matching HTTP status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid",
            message: msg.into(),
        }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: msg.into(),
        }
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: msg.into(),
        }
    }
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "unavailable",
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn ok(v: Value) -> ApiResult {
    Ok(Json(v))
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

// ───────────────────────────────────────────────────────────────────────────
// Router
// ───────────────────────────────────────────────────────────────────────────

/// The `/api/v2` sub-router (nested by `router.rs`). `/pair` is open; every
/// other route carries the bearer gate as a route layer.
pub fn routes() -> Router<MobileState> {
    let open = Router::new().route("/pair", post(pair));
    let gated = Router::new()
        .route("/capabilities", get(capabilities))
        .route("/devices", get(devices_list))
        .route("/devices/:id", delete(devices_revoke))
        .route("/threads", get(threads_list).post(threads_create))
        .route("/threads/:id", patch(threads_patch).delete(threads_delete))
        .route("/threads/:id/messages", get(thread_messages))
        .route("/threads/:id/send", post(thread_send))
        .route("/runs", get(runs_list))
        .route("/runs/:id/timeline", get(run_timeline))
        .route("/runs/:id/stop", post(run_stop))
        .route("/approvals", get(approvals_list))
        .route("/approvals/:id", post(approvals_resolve))
        .route("/projects", get(projects_list))
        .route("/projects/add", post(projects_add))
        .route("/projects/discover", get(projects_discover))
        .route("/projects/git/status", get(git_status))
        .route("/projects/git/diff", get(git_diff))
        .route(
            "/checkpoints",
            get(checkpoints_list).post(checkpoints_create),
        )
        .route("/checkpoints/:id/restore", post(checkpoints_restore))
        .route("/reliability", get(reliability))
        .route("/usage", get(usage))
        .route("/routines", get(routines_list).post(routines_create))
        .route(
            "/routines/:id",
            patch(routines_patch).delete(routines_delete),
        )
        .route("/routines/:id/run", post(routines_run))
        .route("/routines/:id/history", get(routines_history))
        .route("/models", get(models))
        .route("/settings/mobile", get(settings_get).put(settings_put))
        .route("/push/status", get(push_status))
        .route("/push/vapid", get(push_vapid))
        .route(
            "/push/subscriptions",
            get(push_subs_list).post(push_subs_add),
        )
        .route("/push/subscriptions/:id", delete(push_subs_remove))
        // POST form for clients whose fetch layer can't send DELETE bodies.
        .route(
            "/push/subscriptions/delete",
            post(push_subs_remove_by_endpoint),
        )
        .route_layer(axum::middleware::from_fn(auth::v2_gate));
    open.merge(gated)
}

// ───────────────────────────────────────────────────────────────────────────
// Pairing + devices + capabilities
// ───────────────────────────────────────────────────────────────────────────

/// Human name of this server: MagicDNS host label → OS hostname → "cortex".
pub fn server_name() -> String {
    if let crate::tailscale::TsStatus::Connected { dnsname, .. } =
        crate::tailscale::current_status()
    {
        if let Some(label) = dnsname.split('.').next().filter(|l| !l.is_empty()) {
            return label.to_string();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "cortex".to_string())
}

#[derive(Debug, Deserialize)]
pub struct PairBody {
    pub code: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

/// `POST /pair { code, device_name }` → `{ token, device_id, server_name,
/// server_version }`. 401 for a wrong/expired/used code.
pub async fn pair(Json(body): Json<PairBody>) -> ApiResult {
    if !pairing::redeem_code(&body.code) {
        return Err(ApiError::unauthorized(
            "invalid or expired pairing code — generate a new one on the desktop",
        ));
    }
    let (dev, token) = pairing::register_device(body.device_name.as_deref().unwrap_or(""))
        .map_err(ApiError::internal)?;
    ok(json!({
        "token": token,
        "device_id": dev.id,
        "device_name": dev.name,
        "server_name": server_name(),
        "server_version": env!("CARGO_PKG_VERSION"),
    }))
}

pub async fn devices_list() -> ApiResult {
    ok(json!({ "devices": pairing::list_devices() }))
}

pub async fn devices_revoke(AxPath(id): AxPath<String>) -> ApiResult {
    match pairing::revoke_device(&id).map_err(ApiError::internal)? {
        true => ok(json!({ "ok": true, "id": id })),
        false => Err(ApiError::not_found(format!("no device '{id}'"))),
    }
}

/// Ids of the local CLI agents that are installed + available.
fn local_agents(app: &AppState) -> Vec<String> {
    let cli_ids: Vec<&str> = crate::agents::ALL_CLI_SPECS.iter().map(|s| s.id).collect();
    let mut out: Vec<String> = app
        .registry
        .read()
        .list_descriptors()
        .into_iter()
        .filter(|d| d.available && (cli_ids.contains(&d.id.as_str()) || d.id.ends_with("-cli")))
        .map(|d| d.id)
        .collect();
    out.sort();
    out
}

pub async fn capabilities(
    State(state): State<MobileState>,
    Extension(access): Extension<Access>,
) -> ApiResult {
    let gateway = !state.app.config.read().gateway_base_url.trim().is_empty();
    let device = match &access {
        Access::Device(d) => Some(json!({ "id": d.id, "name": d.name })),
        Access::Local => None,
    };
    ok(json!({
        "server_version": env!("CARGO_PKG_VERSION"),
        "server_name": server_name(),
        "features": FEATURES,
        "local_agents": local_agents(&state.app),
        "gateway": gateway,
        "server_url_https": webpush::server_https_base(),
        "desktop_attached": state.desktop.is_some(),
        "device": device,
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// Threads & chat
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ThreadsQuery {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

pub async fn threads_list(
    State(state): State<MobileState>,
    Query(q): Query<ThreadsQuery>,
) -> ApiResult {
    let cursor =
        match q.cursor.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(c.parse::<i64>().map_err(|_| {
                ApiError::invalid("cursor must be the next_cursor of a previous page")
            })?),
            None => None,
        };
    let limit = q.limit.unwrap_or(threads::DEFAULT_LIMIT);
    let project = q
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let store = state.store.clone();
    let hub = state.v2.clone();
    let project_owned = project.map(str::to_string);
    let (list, next) = tokio::task::spawn_blocking(move || {
        threads::list_threads(&store, &hub, project_owned.as_deref(), limit, cursor)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(json!({ "threads": list, "next_cursor": next }))
}

#[derive(Debug, Deserialize, Default)]
pub struct ThreadCreateBody {
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

fn clean_opt(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn thread_or_404(store: &TracingStore, hub: &V2Hub, id: &str) -> Result<ThreadView, ApiError> {
    threads::thread_view(store, hub, id)
        .ok_or_else(|| ApiError::not_found(format!("no thread '{id}'")))
}

pub async fn threads_create(
    State(state): State<MobileState>,
    body: Option<Json<ThreadCreateBody>>,
) -> ApiResult {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    if let Some(root) = body
        .project_root
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        if !Path::new(root).is_dir() {
            return Err(ApiError::invalid(format!(
                "project_root is not a directory: {root}"
            )));
        }
    }
    let now = now_ms();
    let id = format!("session-{}", uuid::Uuid::new_v4());
    threads::upsert_meta(
        &state.store,
        &threads::ThreadMeta {
            id: id.clone(),
            title: clean_opt(body.title),
            project_root: clean_opt(body.project_root),
            agent_id: clean_opt(body.agent_id),
            model: clean_opt(body.model),
            created_ms: now,
            last_ms: now,
        },
    )
    .map_err(ApiError::internal)?;
    let view = thread_or_404(&state.store, &state.v2, &id)?;
    state.v2.publish(super::events::V2Event::ThreadUpdated {
        thread: view.clone(),
    });
    Ok(Json(to_value(&view)?))
}

#[derive(Debug, Deserialize)]
pub struct ThreadPatchBody {
    pub title: String,
}

pub async fn threads_patch(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<ThreadPatchBody>,
) -> ApiResult {
    thread_or_404(&state.store, &state.v2, &id)?;
    let title = body.title.trim();
    if title.is_empty() {
        return Err(ApiError::invalid("title must not be empty"));
    }
    let title: String = title.chars().take(200).collect();
    threads::set_title(&state.store, &id, &title).map_err(ApiError::internal)?;
    let view = thread_or_404(&state.store, &state.v2, &id)?;
    state.v2.publish(super::events::V2Event::ThreadUpdated {
        thread: view.clone(),
    });
    Ok(Json(to_value(&view)?))
}

pub async fn threads_delete(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
) -> ApiResult {
    if state.v2.is_running(&id) {
        return Err(ApiError::invalid(
            "stop the running turn before deleting the thread",
        ));
    }
    match threads::delete_thread(&state.store, &id).map_err(ApiError::internal)? {
        true => ok(json!({ "ok": true, "id": id })),
        false => Err(ApiError::not_found(format!("no thread '{id}'"))),
    }
}

#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub after: Option<String>,
}

pub async fn thread_messages(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<MessagesQuery>,
) -> ApiResult {
    if !threads::exists(&state.store, &id) && !state.v2.is_running(&id) {
        return Err(ApiError::not_found(format!("no thread '{id}'")));
    }
    let msgs = threads::messages(
        &state.store,
        &state.v2,
        &id,
        q.limit.unwrap_or(200),
        q.before,
        q.after.as_deref().map(str::trim).filter(|a| !a.is_empty()),
    );
    ok(json!({ "messages": msgs, "thread_id": id, "running": state.v2.is_running(&id) }))
}

#[derive(Debug, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct SendBody {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// `"plan"` | `"act"`; defaults to the mobile settings' plan_mode.
    #[serde(default)]
    pub mode: Option<String>,
}

/// Split attachments into image data-URIs (forwarded as `images`) and an
/// inline text block appended to the prompt. Anything else is rejected.
fn shape_attachments(atts: &[Attachment]) -> Result<(Vec<String>, String), ApiError> {
    use base64::Engine;
    let mut images = Vec::new();
    let mut text = String::new();
    for a in atts {
        let mime = a.mime.trim().to_ascii_lowercase();
        let data = a.data_base64.trim();
        if data.is_empty() {
            return Err(ApiError::invalid(format!(
                "attachment '{}' is empty",
                a.name
            )));
        }
        if mime.starts_with("image/") {
            images.push(format!("data:{mime};base64,{data}"));
        } else if mime.starts_with("text/")
            || matches!(
                mime.as_str(),
                "application/json" | "application/x-yaml" | "application/toml"
            )
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| {
                    ApiError::invalid(format!("attachment '{}': bad base64: {e}", a.name))
                })?;
            if bytes.len() > ATTACHMENT_TEXT_CAP {
                return Err(ApiError::invalid(format!(
                    "attachment '{}' too large ({} bytes, cap {ATTACHMENT_TEXT_CAP})",
                    a.name,
                    bytes.len()
                )));
            }
            let body = String::from_utf8_lossy(&bytes);
            text.push_str(&format!(
                "\n\n<attachment name=\"{}\">\n{}\n</attachment>",
                a.name.replace('"', "'"),
                body
            ));
        } else {
            return Err(ApiError::invalid(format!(
                "attachment '{}': unsupported type {mime} (images and text only)",
                a.name
            )));
        }
    }
    Ok((images, text))
}

/// Recent turns of a thread as pipeline history (oldest first).
fn history_for(store: &TracingStore, thread_id: &str) -> Vec<ChatTurn> {
    let mut msgs = store.load_session_messages(thread_id).unwrap_or_default();
    msgs.retain(|m| m.role == "user" || m.role == "assistant");
    let skip = msgs.len().saturating_sub(HISTORY_TURNS);
    msgs.into_iter()
        .skip(skip)
        .map(|m| ChatTurn {
            role: m.role,
            content: m.content,
            agent: m.agent_id,
        })
        .collect()
}

/// `POST /threads/:id/send` → `{ run_id, message_id, thread_id, picked_agents,
/// routing_reason, attachments }`. Streaming follows on `/ws`.
pub async fn thread_send(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<SendBody>,
) -> ApiResult {
    let (images, inline_text) = shape_attachments(&body.attachments)?;
    let content = body.content.trim().to_string();
    if content.is_empty() && images.is_empty() && inline_text.is_empty() {
        return Err(ApiError::invalid("content must not be empty"));
    }
    if state.v2.is_running(&id) {
        return Err(ApiError::invalid(
            "a turn is already running on this thread — stop it or wait for it to finish",
        ));
    }
    let now = now_ms();
    let meta = threads::get_meta(&state.store, &id);
    if meta.is_none() && !threads::exists(&state.store, &id) {
        // A brand-new id from the client: adopt it (desktop sessions are
        // created client-side the same way).
        threads::upsert_meta(
            &state.store,
            &threads::ThreadMeta {
                id: id.clone(),
                created_ms: now,
                last_ms: now,
                ..Default::default()
            },
        )
        .map_err(ApiError::internal)?;
    }
    let settings = load_mobile_settings();
    let project_root: Option<String> = meta
        .as_ref()
        .and_then(|m| m.project_root.clone())
        .or_else(|| {
            state
                .app
                .config
                .read()
                .default_project_root
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .filter(|p| Path::new(p).is_dir());
    let agent = clean_opt(body.agent_id)
        .or_else(|| meta.as_ref().and_then(|m| m.agent_id.clone()))
        .or_else(|| settings.default_agent_id.clone());
    let model = clean_opt(body.model)
        .or_else(|| meta.as_ref().and_then(|m| m.model.clone()))
        .or_else(|| settings.default_model.clone());
    let mode = clean_opt(body.mode)
        .or_else(|| Some(if settings.plan_mode { "plan" } else { "act" }.to_string()));

    let history = history_for(&state.store, &id);
    let (run_id, message_id) =
        state
            .v2
            .begin_run(&id, agent.clone(), model.clone(), project_root.clone());

    // Persist the user turn first so history + the thread list see it even
    // if dispatch fails.
    let user_msg = StoredMessage {
        id: format!("user-{}", ulid::Ulid::new().to_string().to_lowercase()),
        session_id: id.clone(),
        ts: now,
        role: "user".into(),
        agent_id: None,
        content: if inline_text.is_empty() {
            content.clone()
        } else {
            format!("{content}{inline_text}")
        },
        run_id: Some(run_id.clone()),
        reasoning: None,
        project_root: project_root.clone(),
    };
    let _ = state.store.record_message(&user_msg);
    threads::touch(&state.store, &id, now);

    let args = ChatSendArgs {
        session_id: id.clone(),
        message: user_msg.content.clone(),
        agent,
        project_root,
        history,
        mode,
        architect_mode: None,
        planner_model: None,
        editor_model: None,
        images,
        model,
        reasoning_effort: None,
    };
    match chat::chat_send_with(args, state.chat_sink(), state.app.clone()).await {
        Ok(res) => ok(json!({
            "run_id": run_id,
            "message_id": message_id,
            "user_message_id": user_msg.id,
            "thread_id": id,
            "picked_agents": res.picked_agents,
            "routing_reason": res.routing_reason,
            "attachments": res.attachments,
        })),
        Err(e) => {
            state.v2.fail_run(&run_id, &e);
            Err(ApiError::unavailable(e))
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Runs / replay
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub thread_id: Option<String>,
}

fn span_status(s: &str) -> &'static str {
    match s {
        "running" => "running",
        "ok" | "done" => "done",
        "error" | "failed" => "error",
        "stopped" | "cancelled" => "stopped",
        _ => "done",
    }
}

fn cost_for(model: Option<&str>, agent: Option<&str>, tokens: u64) -> Option<f64> {
    if tokens == 0 {
        return None;
    }
    let price = crate::pricing::lookup_price(model.unwrap_or(agent.unwrap_or("")));
    let (p, c) = crate::pricing::split_tokens(tokens);
    Some(crate::pricing::compute_usd(p, c, price))
}

/// Live runs from the hub merged with recorded `agent.run` spans. A finished
/// hub run whose span exists is dropped in favour of the span (it has the
/// timeline); active runs always show.
pub fn merge_runs(live: Vec<RunView>, spans: Vec<RunView>) -> Vec<RunView> {
    let mut out: Vec<RunView> = Vec::new();
    for r in live {
        let covered = r.ended_ms.is_some()
            && spans.iter().any(|s| {
                s.thread_id == r.thread_id
                    && s.started_ms >= r.started_ms - 2_000
                    && s.started_ms <= r.ended_ms.unwrap_or(i64::MAX)
            });
        if !covered {
            out.push(r);
        }
    }
    out.extend(spans);
    out.sort_by(|a, b| b.started_ms.cmp(&a.started_ms));
    out
}

pub async fn runs_list(State(state): State<MobileState>, Query(q): Query<RunsQuery>) -> ApiResult {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let thread = q
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let spans: Vec<RunView> = state
        .store
        .list_replay_runs(thread, limit)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .into_iter()
        .map(|r| RunView {
            run_id: r.span_id,
            thread_id: r.session_id,
            started_ms: r.started_at,
            ended_ms: r.ended_at,
            status: if r.had_error && r.status != "running" {
                "error".into()
            } else {
                span_status(&r.status).into()
            },
            cost_usd: cost_for(r.model.as_deref(), r.agent_id.as_deref(), r.tokens),
            tokens: (r.tokens > 0).then_some(r.tokens),
            agent_id: r.agent_id,
            model: r.model,
        })
        .collect();
    let mut runs = merge_runs(state.v2.runs(thread), spans);
    runs.truncate(limit);
    ok(json!({ "runs": runs }))
}

/// Resolve a `/runs/:id` id to a replay: a span id directly, or a hub run id
/// mapped to the span the pipeline recorded for it.
fn replay_for(
    state: &MobileState,
    id: &str,
) -> Option<crate::observability::tracing_store::RunReplay> {
    if let Ok(r) = state.store.run_replay(id) {
        return Some(r);
    }
    let run = state.v2.run(id)?;
    let spans = state
        .store
        .list_replay_runs(Some(&run.thread_id), 20)
        .unwrap_or_default();
    let span = spans
        .into_iter()
        .filter(|s| s.started_at >= run.started_ms - 2_000)
        .min_by_key(|s| s.started_at)?;
    state.store.run_replay(&span.span_id).ok()
}

pub async fn run_timeline(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
) -> ApiResult {
    let Some(replay) = replay_for(&state, &id) else {
        // A live run with no span yet: synthesize from the hub state.
        if let Some(run) = state.v2.run(&id) {
            let m = run.message();
            let mut events = vec![json!({
                "ts_ms": run.started_ms, "kind": "prompt", "summary": "turn started",
            })];
            if let Some(r) = &run.routing_reason {
                events.push(json!({ "ts_ms": run.started_ms, "kind": "route", "summary": r }));
            }
            for t in &run.tools {
                events.push(json!({
                    "ts_ms": run.started_ms, "kind": "tool_call",
                    "summary": format!("{} {}", t.name, t.args_preview), "detail": t,
                }));
            }
            if let Some(e) = &run.error {
                events.push(json!({ "ts_ms": run.ended_ms.unwrap_or(run.started_ms), "kind": "error", "summary": e }));
            }
            return ok(json!({
                "run_id": id, "thread_id": run.thread_id, "status": run.view().status,
                "agent_id": run.agent_id, "model": run.model, "events": events,
                "live": true, "message": m,
            }));
        }
        return Err(ApiError::not_found(format!("no run '{id}'")));
    };
    let mut events: Vec<Value> = Vec::new();
    events.push(json!({
        "ts_ms": replay.started_at,
        "kind": "prompt",
        "summary": replay.prompt_preview.clone().unwrap_or_else(|| "turn started".into()),
    }));
    if let Some(r) = &replay.routing_reason {
        events.push(json!({ "ts_ms": replay.started_at, "kind": "route", "summary": r }));
    }
    let mut streamed_chars: u64 = 0;
    let mut reasoning_chars: u64 = 0;
    for step in &replay.steps {
        let p = &step.payload;
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let (kind, summary) = match step.name.as_str() {
            "token" => {
                streamed_chars += p.get("chars").and_then(Value::as_u64).unwrap_or(0);
                continue;
            }
            "reasoning" => {
                reasoning_chars += p.get("chars").and_then(Value::as_u64).unwrap_or(0);
                continue;
            }
            "started" => continue,
            "tool_call" => (
                "tool_call",
                format!("{} {}", s("name"), s("preview")).trim().to_string(),
            ),
            "tool_result" => (
                "tool_result",
                format!(
                    "{} {}",
                    s("name"),
                    if p.get("ok").and_then(Value::as_bool).unwrap_or(true) {
                        "ok"
                    } else {
                        "failed"
                    }
                ),
            ),
            "file_edit" => (
                "edit",
                format!(
                    "{} ({} lines)",
                    s("path"),
                    p.get("lines").and_then(Value::as_i64).unwrap_or(0)
                ),
            ),
            "approval_request" => ("approval", format!("approval requested: {}", s("tool"))),
            "approval_resolved" => ("approval", format!("approval {}", s("choice"))),
            "error" => ("error", s("message")),
            "done" => (
                "result",
                format!(
                    "done · {} tokens · {} chars streamed",
                    p.get("tokens").and_then(Value::as_u64).unwrap_or(0),
                    streamed_chars
                ),
            ),
            other => ("result", other.to_string()),
        };
        events.push(json!({ "ts_ms": step.ts, "kind": kind, "summary": summary, "detail": p }));
    }
    ok(json!({
        "run_id": replay.span_id,
        "thread_id": replay.session_id,
        "trace_id": replay.trace_id,
        "status": span_status(&replay.status),
        "agent_id": replay.agent_id,
        "model": replay.model,
        "started_ms": replay.started_at,
        "ended_ms": replay.ended_at,
        "total_tokens": replay.total_tokens,
        "cost_usd": replay.est_usd,
        "streamed_chars": streamed_chars,
        "reasoning_chars": reasoning_chars,
        "events": events,
    }))
}

/// Stop an underlying agent run: local runs are aborted in-process, anything
/// else goes to the gateway (same order as the desktop `stop_run`).
async fn stop_agent_run(app: &AppState, agent_run_id: &str) -> Result<(), String> {
    if chat::abort_local_run(agent_run_id) || agent_run_id.starts_with("local-") {
        return Ok(());
    }
    let cfg = app.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    GatewayClient::new(cfg.gateway_base_url, api_key)
        .stop_run(agent_run_id)
        .await
        .map_err(|e| e.to_string())
}

pub async fn run_stop(State(state): State<MobileState>, AxPath(id): AxPath<String>) -> ApiResult {
    let targets = match state.v2.mark_stopped(&id) {
        Some(ids) if !ids.is_empty() => ids,
        // Known to the hub but no agent run id seen yet (still routing) —
        // nothing to abort; the pipeline will finish on its own.
        Some(_) => return ok(json!({ "ok": true, "run_id": id, "aborted": [] })),
        // Not a hub run: treat the id as an agent/gateway run id directly.
        None => vec![id.clone()],
    };
    let mut aborted = Vec::new();
    let mut errors = Vec::new();
    for t in &targets {
        match stop_agent_run(&state.app, t).await {
            Ok(()) => aborted.push(t.clone()),
            Err(e) => errors.push(format!("{t}: {e}")),
        }
    }
    if aborted.is_empty() && !errors.is_empty() {
        return Err(ApiError::unavailable(errors.join("; ")));
    }
    ok(json!({ "ok": true, "run_id": id, "aborted": aborted, "errors": errors }))
}

// ───────────────────────────────────────────────────────────────────────────
// Approvals
// ───────────────────────────────────────────────────────────────────────────

pub async fn approvals_list(State(state): State<MobileState>) -> ApiResult {
    ok(json!({ "approvals": state.v2.pending_approvals() }))
}

#[derive(Debug, Deserialize)]
pub struct ApprovalBody {
    pub decision: String,
    #[serde(default)]
    pub remember: Option<bool>,
}

pub async fn approvals_resolve(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<ApprovalBody>,
) -> ApiResult {
    let decision = match body.decision.trim().to_ascii_lowercase().as_str() {
        "approve" | "approved" | "once" | "always" | "yes" => "approve",
        "deny" | "denied" | "no" => "deny",
        other => {
            return Err(ApiError::invalid(format!(
                "decision must be approve|deny (got {other:?})"
            )))
        }
    };
    let Some(approval) = state.v2.approval(&id) else {
        return Err(ApiError::not_found(format!("no pending approval '{id}'")));
    };
    if approval.resolved {
        return Err(ApiError::invalid("approval already resolved"));
    }
    let remember = body.remember.unwrap_or(false) && decision == "approve";
    // Local (in-process) waits resolve directly; everything else is a gateway
    // run id. Same vocabulary the desktop sends: once/always/deny.
    let gateway_choice = if decision == "deny" {
        "deny"
    } else if remember {
        "always"
    } else {
        "once"
    };
    if !chat::resolve_local_approval(&id, gateway_choice) {
        let cfg = state.app.config.read().clone();
        let api_key = AppState::get_gateway_api_key().unwrap_or_default();
        GatewayClient::new(cfg.gateway_base_url, api_key)
            .approve_run(&id, gateway_choice, None, None, None)
            .await
            .map_err(|e| {
                ApiError::unavailable(format!("could not deliver the decision to the run: {e}"))
            })?;
    }
    // Keep the legacy pending list in step.
    state.approvals.lock().retain(|a| a.id != id);
    if remember {
        let _ = crate::orchestrator::AutoApproveList::add(crate::orchestrator::AutoApproveEntry {
            tool: approval.tool.clone(),
            pattern: "*".into(),
            profile: None,
        });
    }
    state.v2.mark_approval_resolved(&id, decision);
    ok(json!({ "ok": true, "id": id, "decision": decision, "remembered": remember }))
}

// ───────────────────────────────────────────────────────────────────────────
// Projects / git / checkpoints
// ───────────────────────────────────────────────────────────────────────────

/// Current branch from `.git/HEAD` without shelling out (cheap enough to do
/// per project in a list).
pub fn git_branch_of(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref: ") {
        return Some(r.rsplit('/').next().unwrap_or(r).to_string());
    }
    (!head.is_empty()).then(|| head.chars().take(12).collect())
}

fn discover(state: &MobileState) -> Vec<crate::projects::ProjectMeta> {
    let vault = state.app.config.read().obsidian_vault.clone();
    crate::projects::discover_projects(vault)
}

pub async fn projects_list(State(state): State<MobileState>) -> ApiResult {
    let st = state.clone();
    let projects = tokio::task::spawn_blocking(move || {
        discover(&st)
            .into_iter()
            .map(|p| {
                let trusted = crate::orchestrator::trust::is_trusted(&p.root);
                let branch = if p.has_git {
                    git_branch_of(&p.root)
                } else {
                    None
                };
                json!({
                    "root": p.root.to_string_lossy(),
                    "name": p.name,
                    "trusted": trusted,
                    "branch": branch,
                    "has_git": p.has_git,
                    "kind": p.kind,
                    "group": p.group,
                    "last_opened_ms": p.last_modified_ms,
                })
            })
            .collect::<Vec<Value>>()
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(json!({ "projects": projects }))
}

#[derive(Debug, Deserialize)]
pub struct AddProjectBody {
    pub root: String,
}

pub async fn projects_add(Json(body): Json<AddProjectBody>) -> ApiResult {
    let root = PathBuf::from(body.root.trim());
    if !root.is_dir() {
        return Err(ApiError::invalid(format!("not a directory: {}", body.root)));
    }
    let added = tokio::task::spawn_blocking(move || crate::projects::register_project_path(&root))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    ok(json!({ "ok": true, "added": added }))
}

/// Candidate roots the phone can add without browsing the disk: git repos one
/// level under the usual code folders, flagged when already discovered.
pub async fn projects_discover(State(state): State<MobileState>) -> ApiResult {
    let st = state.clone();
    let roots = tokio::task::spawn_blocking(move || {
        let known: Vec<PathBuf> = discover(&st)
            .into_iter()
            .map(|p| crate::paths::canonicalize_lossy(&p.root))
            .collect();
        let mut out: Vec<Value> = Vec::new();
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut bases: Vec<PathBuf> = Vec::new();
        if let Some(home) = crate::paths::home_dir() {
            for d in ["projects", "code", "src", "dev", "repos", "git", "work"] {
                bases.push(home.join(d));
            }
        }
        if let Some(docs) = crate::paths::documents_dir() {
            bases.push(docs.join("GitHub"));
            bases.push(docs.join("Projects"));
        }
        if let Some(root) = st.app.config.read().default_project_root.clone() {
            bases.push(root);
        }
        for base in bases {
            let Ok(rd) = std::fs::read_dir(&base) else {
                continue;
            };
            for entry in rd.flatten().take(500) {
                let p = entry.path();
                if !p.is_dir() || !p.join(".git").exists() {
                    continue;
                }
                let canon = crate::paths::canonicalize_lossy(&p);
                if !seen.insert(canon.clone()) {
                    continue;
                }
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.starts_with('.') {
                    continue;
                }
                out.push(json!({
                    "root": canon.to_string_lossy(),
                    "name": name,
                    "already_added": known.iter().any(|k| *k == canon),
                }));
            }
        }
        out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        out
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(json!({ "roots": roots }))
}

/// Validate a caller-supplied project root: must be an existing directory
/// that Cortex itself discovers (same rule as the MCP checkpoint tools).
async fn known_root(state: &MobileState, raw: &str) -> Result<PathBuf, ApiError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::invalid("root is required"));
    }
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(ApiError::invalid(format!("not a directory: {raw}")));
    }
    let canon = crate::paths::canonicalize_lossy(&path);
    let st = state.clone();
    let allowed = tokio::task::spawn_blocking(move || {
        discover(&st)
            .into_iter()
            .any(|p| crate::paths::canonicalize_lossy(&p.root) == canon)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    if allowed {
        Ok(crate::paths::canonicalize_lossy(&path))
    } else {
        Err(ApiError::invalid(format!(
            "root is not a registered Cortex project: {raw} (add it via /projects/add)"
        )))
    }
}

#[derive(Debug, Deserialize)]
pub struct RootQuery {
    pub root: String,
}

pub async fn git_status(State(state): State<MobileState>, Query(q): Query<RootQuery>) -> ApiResult {
    let root = known_root(&state, &q.root).await?;
    let status = tokio::task::spawn_blocking(move || crate::git::working_status(&root))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::unavailable)?;
    let mut files: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (entries, area) in [(&status.staged, "staged"), (&status.unstaged, "unstaged")] {
        for f in entries {
            if seen.insert(f.path.clone()) {
                files.push(json!({ "path": f.path, "status": f.status, "area": area }));
            }
        }
    }
    for p in &status.untracked {
        if seen.insert(p.clone()) {
            files.push(json!({ "path": p, "status": "?", "area": "untracked" }));
        }
    }
    ok(json!({
        "branch": status.branch,
        "ahead": status.ahead,
        "behind": status.behind,
        "files": files,
    }))
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub root: String,
    pub path: String,
    #[serde(default)]
    pub mode: Option<String>,
}

/// A repo-relative path that cannot escape the root.
fn safe_rel_path(p: &str) -> Result<String, ApiError> {
    let p = p.trim();
    if p.is_empty() {
        return Err(ApiError::invalid("path is required"));
    }
    let path = Path::new(p);
    // `has_root` catches "/abs" on Windows too, where a rootless drive-less
    // path is not `is_absolute()` but still escapes the project root.
    if path.is_absolute()
        || path.has_root()
        || p.starts_with(['/', '\\'])
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ApiError::invalid(
            "path must be relative to the project root",
        ));
    }
    Ok(p.to_string())
}

pub async fn git_diff(State(state): State<MobileState>, Query(q): Query<DiffQuery>) -> ApiResult {
    let root = known_root(&state, &q.root).await?;
    let rel = safe_rel_path(&q.path)?;
    let mode = q.mode.clone();
    let diff = tokio::task::spawn_blocking(move || -> Result<String, String> {
        use crate::git::DiffMode;
        match mode.as_deref() {
            Some(m) => crate::git::file_diff(&root, &rel, DiffMode::parse(m)?),
            None => {
                // Auto: working tree first, then index, then untracked.
                let unstaged = crate::git::file_diff(&root, &rel, DiffMode::Unstaged)?;
                if !unstaged.trim().is_empty() {
                    return Ok(unstaged);
                }
                let staged = crate::git::file_diff(&root, &rel, DiffMode::Staged)?;
                if !staged.trim().is_empty() {
                    return Ok(staged);
                }
                crate::git::file_diff(&root, &rel, DiffMode::Untracked)
            }
        }
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::unavailable)?;
    let (diff, truncated) = cap_text(diff, DIFF_CAP_BYTES);
    ok(json!({ "diff": diff, "truncated": truncated, "path": q.path }))
}

/// Cut `s` to at most `cap` bytes on a char boundary.
pub fn cap_text(s: String, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    let mut cut = cap;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (s[..cut].to_string(), true)
}

pub async fn checkpoints_list(
    State(state): State<MobileState>,
    Query(q): Query<RootQuery>,
) -> ApiResult {
    let root = known_root(&state, &q.root).await?;
    let list = tokio::task::spawn_blocking(move || {
        crate::commands::checkpoints::list_checkpoints_sync(&root)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::internal)?;
    ok(json!({ "checkpoints": list }))
}

#[derive(Debug, Deserialize)]
pub struct CheckpointCreateBody {
    pub root: String,
    #[serde(default)]
    pub label: Option<String>,
}

pub async fn checkpoints_create(
    State(state): State<MobileState>,
    Json(body): Json<CheckpointCreateBody>,
) -> ApiResult {
    let root = known_root(&state, &body.root).await?;
    let label = clean_opt(body.label);
    let info = tokio::task::spawn_blocking(move || {
        crate::commands::checkpoints::make_checkpoint(&root, label)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::invalid)?;
    Ok(Json(to_value(&info)?))
}

#[derive(Debug, Deserialize)]
pub struct CheckpointRestoreBody {
    pub root: String,
    #[serde(default)]
    pub force: Option<bool>,
}

/// Destructive: overwrites the working tree. Requires `X-Confirm: restore`.
pub async fn checkpoints_restore(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    Json(body): Json<CheckpointRestoreBody>,
) -> ApiResult {
    let confirmed = headers
        .get("x-confirm")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("restore"))
        .unwrap_or(false);
    if !confirmed {
        return Err(ApiError::invalid(
            "destructive: send the `X-Confirm: restore` header to overwrite the working tree",
        ));
    }
    let root = known_root(&state, &body.root).await?;
    let force = body.force.unwrap_or(false);
    let rid = id.clone();
    tokio::task::spawn_blocking(move || {
        crate::commands::checkpoints::restore_checkpoint_core(&root, &rid, force)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::invalid)?;
    ok(json!({ "ok": true, "id": id }))
}

// ───────────────────────────────────────────────────────────────────────────
// Reliability / usage
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    #[serde(default)]
    pub range: Option<String>,
}

/// `7d` / `24h` / `30m` / `all` → hours (None = all time).
pub fn parse_range_hours(range: Option<&str>) -> Result<Option<u32>, String> {
    let r = range.map(str::trim).unwrap_or("7d").to_ascii_lowercase();
    if r.is_empty() || r == "all" {
        return Ok(None);
    }
    let (num, unit) = r.split_at(r.len() - 1);
    let n: u32 = num.parse().map_err(|_| format!("bad range: {r}"))?;
    let hours = match unit {
        "h" => n,
        "d" => n.saturating_mul(24),
        "w" => n.saturating_mul(24 * 7),
        "m" => n.div_ceil(60).max(1),
        _ => return Err(format!("bad range unit in: {r} (use h/d/w)")),
    };
    Ok(Some(hours.min(24 * 365)))
}

pub async fn reliability(
    State(state): State<MobileState>,
    Query(q): Query<RangeQuery>,
) -> ApiResult {
    let hours = parse_range_hours(q.range.as_deref()).map_err(ApiError::invalid)?;
    let since_ms = hours.map(|h| now_ms() - (h as i64) * 3_600_000);
    let report = state
        .store
        .reliability_summary(since_ms)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut v = to_value(&report)?;
    if let Value::Object(map) = &mut v {
        map.insert(
            "range".into(),
            Value::String(q.range.clone().unwrap_or_else(|| "7d".into())),
        );
    }
    Ok(Json(v))
}

/// RFC 3339 → unix ms (`None` when unparseable).
fn rfc3339_ms(s: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s?.trim())
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Seconds or ms (heuristic on magnitude) → ms.
fn epoch_ms(v: i64) -> Option<i64> {
    if v <= 0 {
        None
    } else if v < 100_000_000_000 {
        Some(v * 1000)
    } else {
        Some(v)
    }
}

pub async fn usage(State(state): State<MobileState>) -> ApiResult {
    let acct = crate::commands::account_usage::account_usage()
        .await
        .unwrap_or(crate::commands::account_usage::AccountUsage {
            claude: None,
            chatgpt: None,
        });
    let claude = acct.claude.as_ref().map(|c| {
        json!({
            "five_hour_pct": c.five_hour_pct,
            "seven_day_pct": c.seven_day_pct,
            "resets_ms": rfc3339_ms(c.five_hour_resets_at.as_deref()),
            "seven_day_resets_ms": rfc3339_ms(c.seven_day_resets_at.as_deref()),
            "sonnet_pct": c.sonnet_pct,
        })
    });
    let chatgpt = acct.chatgpt.as_ref().map(|c| {
        json!({
            "plan": c.plan_type,
            "five_hour_pct": c.primary_used_pct,
            "seven_day_pct": c.secondary_used_pct,
            "resets_ms": epoch_ms(c.primary_reset_at),
            "seven_day_resets_ms": epoch_ms(c.secondary_reset_at),
            "limit_reached": c.limit_reached,
        })
    });
    // Local spend estimate: per-provider token totals × the pricing table
    // (the same rollup the desktop cost tracker shows).
    let spent_usd: f64 = state
        .store
        .tokens_by_provider(100)
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let price = crate::pricing::lookup_price(&p.agent_id);
            let (i, o) = crate::pricing::split_tokens(p.total_tokens);
            crate::pricing::compute_usd(i, o, price)
        })
        .sum();
    // Quota push (opt-in via subscriptions; deduped per reset window).
    if let Some(c) = &acct.claude {
        let pct = c.five_hour_pct.max(c.seven_day_pct);
        if pct >= 90.0 {
            webpush::fire_detached(
                format!(
                    "quota:{}",
                    c.five_hour_resets_at.clone().unwrap_or_default()
                ),
                webpush::Notification {
                    event: webpush::EVENT_QUOTA_LOW,
                    title: "Claude quota nearly used".into(),
                    body: format!("{pct:.0}% of the window is used"),
                    target_id: None,
                    thread_id: None,
                    run_id: None,
                    app_badge: None,
                },
            );
        }
    }
    ok(json!({
        "claude": claude,
        "chatgpt": chatgpt,
        "budget": { "spent_usd": spent_usd, "cap_usd": Value::Null },
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// Routines
// ───────────────────────────────────────────────────────────────────────────

fn routine_or_404(id: &str) -> Result<RoutineSpec, ApiError> {
    routines::list_routines()
        .map_err(ApiError::internal)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| ApiError::not_found(format!("no routine '{id}'")))
}

pub async fn routines_list() -> ApiResult {
    let list = routines::list_routines().map_err(ApiError::internal)?;
    ok(json!({ "routines": list }))
}

pub async fn routines_create(Json(mut spec): Json<RoutineSpec>) -> ApiResult {
    if spec.id.trim().is_empty() {
        spec.id = format!("routine-{}", uuid::Uuid::new_v4());
    }
    spec.next_run_unix_ms = None;
    let id = spec.id.clone();
    routines::save_routine(spec).map_err(ApiError::invalid)?;
    Ok(Json(to_value(&routine_or_404(&id)?)?))
}

/// Editable fields only: the run bookkeeping (`last_*`) is server-owned.
const ROUTINE_PATCH_FIELDS: &[&str] = &[
    "name",
    "prompt",
    "interval_minutes",
    "enabled",
    "agent_id",
    "project_root",
    "daily_at",
];

pub async fn routines_patch(AxPath(id): AxPath<String>, Json(body): Json<Value>) -> ApiResult {
    let current = routine_or_404(&id)?;
    let Value::Object(patch) = body else {
        return Err(ApiError::invalid("body must be a JSON object"));
    };
    let mut merged = to_value(&current)?;
    if let Value::Object(m) = &mut merged {
        for (k, v) in patch {
            if ROUTINE_PATCH_FIELDS.contains(&k.as_str()) {
                m.insert(k, v);
            }
        }
    }
    let mut spec: RoutineSpec =
        serde_json::from_value(merged).map_err(|e| ApiError::invalid(e.to_string()))?;
    spec.id = id.clone();
    routines::save_routine(spec).map_err(ApiError::invalid)?;
    Ok(Json(to_value(&routine_or_404(&id)?)?))
}

pub async fn routines_delete(AxPath(id): AxPath<String>) -> ApiResult {
    routine_or_404(&id)?;
    routines::delete_routine(id.clone()).map_err(ApiError::internal)?;
    ok(json!({ "ok": true, "id": id }))
}

/// Kick off a run and return immediately (a local CLI run can take minutes);
/// the outcome lands in `/routines/:id/history` and, on the desktop, in the
/// Routines panel.
pub async fn routines_run(
    State(state): State<MobileState>,
    AxPath(id): AxPath<String>,
) -> ApiResult {
    routine_or_404(&id)?;
    let st = state.clone();
    let rid = id.clone();
    tokio::spawn(async move {
        let result = match &st.desktop {
            Some(app) => routines::execute_routine(app, &rid, "manual")
                .await
                .map(|_| ()),
            None => routines::run_and_record(&st.app, &st.store, &rid, "manual")
                .await
                .map(|(_, run)| routines::notify_run_outcome(&run)),
        };
        if let Err(e) = result {
            tracing::warn!("routine {rid} (mobile trigger) failed: {e}");
        }
    });
    Ok(Json(json!({ "accepted": true, "routine_id": id })))
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn routines_history(
    AxPath(id): AxPath<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult {
    let runs = routines::list_routine_runs(Some(id.clone()), q.limit.map(|l| l.clamp(1, 200)))
        .map_err(ApiError::internal)?;
    ok(json!({ "runs": runs, "routine_id": id }))
}

// ───────────────────────────────────────────────────────────────────────────
// Models
// ───────────────────────────────────────────────────────────────────────────

fn cost_tier(id: &str) -> &'static str {
    let m = id.to_ascii_lowercase();
    if m.starts_with("ollama") {
        "free"
    } else if ["haiku", "mini", "nano", "flash", "lite", "small"]
        .iter()
        .any(|k| m.contains(k))
    {
        "low"
    } else if ["opus", "gpt-5", "o1", "o3", "pro", "ultra", "large"]
        .iter()
        .any(|k| m.contains(k))
    {
        "high"
    } else {
        "mid"
    }
}

fn provider_of(id: &str, source: &str) -> &'static str {
    let m = id.to_ascii_lowercase();
    if source == "ollama" || m.starts_with("ollama") {
        "ollama"
    } else if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("gemini") {
        "google"
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.contains("codex")
    {
        "openai"
    } else if source == "claude-cli" {
        "anthropic"
    } else {
        "gateway"
    }
}

pub async fn models(State(state): State<MobileState>) -> ApiResult {
    use crate::orchestrator::aliases::CATALOG;
    let (available, gateway_configured, cfg_ollama) = {
        let reg = state.app.registry.read();
        let cfg = state.app.config.read();
        let avail: std::collections::HashSet<String> = reg
            .list_descriptors()
            .into_iter()
            .filter(|d| d.available)
            .map(|d| d.id)
            .collect();
        (
            avail,
            !cfg.gateway_base_url.trim().is_empty(),
            cfg.ollama_base_url.trim_end_matches('/').to_string(),
        )
    };
    let claude_present = crate::agents::claude_cli::claude_bin().is_some();
    let mut out: Vec<Value> = Vec::new();
    for m in CATALOG {
        let cli_ok = m.cli_source.is_some_and(|c| available.contains(c));
        let (local, served_by, is_available) = if m.source == "claude-cli" {
            (true, "claude-cli", claude_present)
        } else if cli_ok {
            (true, m.cli_source.unwrap_or(m.source), true)
        } else {
            (false, m.source, gateway_configured)
        };
        if !is_available {
            continue;
        }
        out.push(json!({
            "id": m.id,
            "label": m.label,
            "provider": provider_of(m.id, m.source),
            "capabilities": ["chat", "exec", "edit"],
            "local": local,
            "cost_tier": cost_tier(m.id),
            "agent_id": served_by,
        }));
    }
    // Ollama tags (best-effort, both the configured server and the local one).
    let mut bases: Vec<String> = Vec::new();
    if !cfg_ollama.is_empty() {
        bases.push(cfg_ollama.clone());
    }
    if cfg_ollama != crate::agents::ollama::LOCAL_OLLAMA {
        bases.push(crate::agents::ollama::LOCAL_OLLAMA.to_string());
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for base in bases {
        let tags = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            crate::agents::ollama::fetch_tags_at(&base),
        )
        .await
        .unwrap_or_default();
        for name in tags {
            if seen.insert(name.clone()) {
                out.push(json!({
                    "id": format!("ollama:{name}"),
                    "label": name,
                    "provider": "ollama",
                    "capabilities": ["chat"],
                    "local": true,
                    "cost_tier": "free",
                    "agent_id": "ollama",
                }));
            }
        }
    }
    let settings = load_mobile_settings();
    let default = settings.default_model.clone().or_else(|| {
        out.first()
            .and_then(|m| m["id"].as_str())
            .map(str::to_string)
    });
    ok(json!({ "models": out, "default": default }))
}

// ───────────────────────────────────────────────────────────────────────────
// Settings (mobile subset)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MobileSettings {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_agent_id: Option<String>,
    #[serde(default)]
    pub plan_mode: bool,
}

fn mobile_settings_path() -> Option<PathBuf> {
    crate::paths::cortex_dir().map(|d| d.join("mobile-settings.json"))
}

pub fn load_mobile_settings() -> MobileSettings {
    let Some(p) = mobile_settings_path() else {
        return MobileSettings::default();
    };
    std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_mobile_settings(s: &MobileSettings) -> Result<(), String> {
    let p = mobile_settings_path().ok_or_else(|| "no home dir".to_string())?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn settings_view(state: &MobileState, s: &MobileSettings) -> Value {
    json!({
        "default_model": s.default_model,
        "default_agent_id": s.default_agent_id,
        "plan_mode": s.plan_mode,
        "sandbox_tier": state.app.config.read().sandbox_tier,
    })
}

pub async fn settings_get(State(state): State<MobileState>) -> ApiResult {
    ok(settings_view(&state, &load_mobile_settings()))
}

#[derive(Debug, Deserialize)]
pub struct SettingsPutBody {
    #[serde(default)]
    pub default_model: Option<Option<String>>,
    #[serde(default)]
    pub default_agent_id: Option<Option<String>>,
    #[serde(default)]
    pub plan_mode: Option<bool>,
    #[serde(default)]
    pub sandbox_tier: Option<Option<String>>,
}

pub async fn settings_put(
    State(state): State<MobileState>,
    Json(body): Json<SettingsPutBody>,
) -> ApiResult {
    let mut s = load_mobile_settings();
    if let Some(v) = body.default_model {
        s.default_model = clean_opt(v);
    }
    if let Some(v) = body.default_agent_id {
        s.default_agent_id = clean_opt(v);
    }
    if let Some(p) = body.plan_mode {
        s.plan_mode = p;
        chat::set_current_mode(if p { "plan" } else { "act" }.to_string())
            .await
            .map_err(ApiError::internal)?;
    }
    if let Some(t) = body.sandbox_tier {
        let t = clean_opt(t);
        if let Some(t) = &t {
            if !matches!(
                t.as_str(),
                "read-only" | "workspace-write" | "danger-full-access"
            ) {
                return Err(ApiError::invalid(
                    "sandbox_tier must be read-only | workspace-write | danger-full-access",
                ));
            }
        }
        state.app.config.write().sandbox_tier = t;
    }
    save_mobile_settings(&s).map_err(ApiError::internal)?;
    ok(settings_view(&state, &s))
}

// ───────────────────────────────────────────────────────────────────────────
// Push
// ───────────────────────────────────────────────────────────────────────────

pub async fn push_status() -> ApiResult {
    let cfg = crate::commands::push_notify::push_get_config()
        .await
        .map_err(ApiError::internal)?;
    let base = webpush::server_https_base();
    let vapid_configured = matches!(webpush::load_keypair(), Ok(Some(_)));
    let subs = pairing::list_push_subscriptions();
    let mut v = to_value(&cfg)?;
    if let Value::Object(m) = &mut v {
        m.insert(
            "deep_link".into(),
            json!({
                "approvals": webpush::deep_links(base.as_deref(), webpush::EVENT_APPROVAL_NEEDED, Some("<id>")),
                "threads": webpush::deep_links(base.as_deref(), webpush::EVENT_RUN_FINISHED, Some("<id>")),
                "inbox": webpush::deep_links(base.as_deref(), webpush::EVENT_QUOTA_LOW, None),
            }),
        );
        m.insert("deep_link_scheme".into(), Value::String("cortex://".into()));
        m.insert("server_url_https".into(), json!(base));
        m.insert(
            "web_push".into(),
            json!({
                "vapid_configured": vapid_configured,
                "subscriptions": subs.len(),
                "events": [
                    webpush::EVENT_APPROVAL_NEEDED,
                    webpush::EVENT_RUN_FINISHED,
                    webpush::EVENT_RUN_FAILED,
                    webpush::EVENT_QUOTA_LOW,
                ],
            }),
        );
    }
    Ok(Json(v))
}

pub async fn push_vapid() -> ApiResult {
    let kp = webpush::ensure_keypair()
        .await
        .map_err(|e| ApiError::unavailable(format!("VAPID key unavailable (key vault): {e}")))?;
    ok(json!({
        "public_key": webpush::public_key_b64(&kp),
        "subject": webpush::VAPID_SUBJECT,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SubKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Deserialize)]
pub struct SubscribeBody {
    pub endpoint: String,
    pub keys: SubKeys,
    #[serde(default)]
    pub device_id: Option<String>,
}

fn sub_view(s: &pairing::PushSubscription) -> Value {
    json!({
        "id": s.id,
        "device_id": s.device_id,
        "endpoint": s.endpoint,
        "created_ms": s.created_ms,
    })
}

pub async fn push_subs_add(
    Extension(access): Extension<Access>,
    Json(body): Json<SubscribeBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device_id = match (&access, clean_opt(body.device_id)) {
        (Access::Device(d), _) => d.id.clone(),
        (Access::Local, Some(id)) => id,
        (Access::Local, None) => {
            return Err(ApiError::invalid(
                "device_id is required when subscribing from the local host",
            ))
        }
    };
    let endpoint = body.endpoint.trim().to_string();
    webpush::endpoint_allowed(&endpoint).map_err(ApiError::invalid)?;
    // Validate the keys now so a bad subscription can never poison sends.
    webpush::encrypt_payload(&body.keys.p256dh, &body.keys.auth, b"{}")
        .map_err(ApiError::invalid)?;
    let sub = pairing::add_push_subscription(
        &device_id,
        &endpoint,
        body.keys.p256dh.trim(),
        body.keys.auth.trim(),
    )
    .map_err(ApiError::invalid)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "subscription": sub_view(&sub) })),
    ))
}

pub async fn push_subs_list(Extension(access): Extension<Access>) -> ApiResult {
    let subs = match &access {
        Access::Device(d) => pairing::push_subscriptions_for(&d.id),
        Access::Local => pairing::list_push_subscriptions(),
    };
    ok(json!({ "subscriptions": subs.iter().map(sub_view).collect::<Vec<_>>() }))
}

pub async fn push_subs_remove(
    Extension(access): Extension<Access>,
    AxPath(id): AxPath<String>,
) -> ApiResult {
    let scope = match &access {
        Access::Device(d) => Some(d.id.as_str()),
        Access::Local => None,
    };
    match pairing::remove_push_subscription(&id, scope).map_err(ApiError::internal)? {
        true => ok(json!({ "ok": true, "id": id })),
        false => Err(ApiError::not_found(format!("no subscription '{id}'"))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeBody {
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

/// `POST /push/subscriptions/delete { endpoint | id }` — same scoping as the
/// DELETE form: a device can only drop its own subscriptions.
pub async fn push_subs_remove_by_endpoint(
    Extension(access): Extension<Access>,
    Json(body): Json<UnsubscribeBody>,
) -> ApiResult {
    let scope: Option<String> = match &access {
        Access::Device(d) => Some(d.id.clone()),
        Access::Local => None,
    };
    let target = match (clean_opt(body.id), clean_opt(body.endpoint)) {
        (Some(id), _) => Some(id),
        (None, Some(ep)) => pairing::list_push_subscriptions()
            .into_iter()
            .find(|s| s.endpoint == ep && scope.as_deref().map_or(true, |d| s.device_id == d))
            .map(|s| s.id),
        (None, None) => return Err(ApiError::invalid("endpoint or id is required")),
    };
    let Some(id) = target else {
        return Err(ApiError::not_found("no such subscription"));
    };
    match pairing::remove_push_subscription(&id, scope.as_deref()).map_err(ApiError::internal)? {
        true => ok(json!({ "ok": true, "id": id })),
        false => Err(ApiError::not_found("no such subscription")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range_hours(None).unwrap(), Some(24 * 7));
        assert_eq!(parse_range_hours(Some("24h")).unwrap(), Some(24));
        assert_eq!(parse_range_hours(Some("30d")).unwrap(), Some(720));
        assert_eq!(parse_range_hours(Some("2w")).unwrap(), Some(336));
        assert_eq!(parse_range_hours(Some("all")).unwrap(), None);
        assert!(parse_range_hours(Some("7x")).is_err());
        assert!(parse_range_hours(Some("d")).is_err());
    }

    #[test]
    fn diff_cap_and_safe_paths() {
        let (s, t) = cap_text("héllo".repeat(10), 7);
        assert!(t);
        assert!(s.len() <= 7);
        assert!(s.starts_with("hé"));
        assert_eq!(cap_text("ok".into(), 10), ("ok".to_string(), false));
        assert!(safe_rel_path("src/main.rs").is_ok());
        assert!(safe_rel_path("../etc/passwd").is_err());
        assert!(safe_rel_path("/abs").is_err());
        assert!(safe_rel_path("").is_err());
    }

    #[test]
    fn model_heuristics() {
        assert_eq!(cost_tier("claude-opus-4-8"), "high");
        assert_eq!(cost_tier("claude-haiku-4-5"), "low");
        assert_eq!(cost_tier("claude-sonnet-4-6"), "mid");
        assert_eq!(cost_tier("ollama:llama3"), "free");
        assert_eq!(provider_of("gemini-2.5-pro", "gateway"), "google");
        assert_eq!(provider_of("gpt-5.5", "gateway"), "openai");
        assert_eq!(provider_of("claude-sonnet-4-6", "claude-cli"), "anthropic");
        assert_eq!(provider_of("x", "ollama"), "ollama");
    }

    #[test]
    fn merge_runs_prefers_spans_for_finished_hub_runs() {
        let live = |id: &str, started: i64, ended: Option<i64>| RunView {
            run_id: id.into(),
            thread_id: "t".into(),
            started_ms: started,
            ended_ms: ended,
            status: if ended.is_some() { "done" } else { "running" }.into(),
            agent_id: None,
            model: None,
            cost_usd: None,
            tokens: None,
        };
        let spans = vec![live("span-1", 1_000, Some(2_000))];
        let merged = merge_runs(
            vec![live("run-a", 900, Some(2_100)), live("run-b", 5_000, None)],
            spans,
        );
        let ids: Vec<&str> = merged.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, vec!["run-b", "span-1"]);
    }

    #[test]
    fn attachments_shape_images_and_text() {
        use base64::Engine;
        let text = base64::engine::general_purpose::STANDARD.encode("hello");
        let (images, inline) = shape_attachments(&[
            Attachment {
                name: "a.png".into(),
                mime: "image/png".into(),
                data_base64: "AAAA".into(),
            },
            Attachment {
                name: "n.txt".into(),
                mime: "text/plain".into(),
                data_base64: text,
            },
        ])
        .unwrap();
        assert_eq!(images, vec!["data:image/png;base64,AAAA".to_string()]);
        assert!(inline.contains("<attachment name=\"n.txt\">\nhello\n</attachment>"));
        assert!(shape_attachments(&[Attachment {
            name: "x.bin".into(),
            mime: "application/octet-stream".into(),
            data_base64: "AAAA".into(),
        }])
        .is_err());
    }

    #[test]
    fn git_branch_from_head_file() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(git_branch_of(tmp.path()).as_deref(), Some("x"));
        std::fs::write(git.join("HEAD"), "0123456789abcdef0123\n").unwrap();
        assert_eq!(git_branch_of(tmp.path()).as_deref(), Some("0123456789ab"));
        assert!(git_branch_of(&tmp.path().join("nope")).is_none());
    }

    #[test]
    fn mobile_settings_round_trip() {
        crate::paths::test_home::with_temp_home(|_| {
            assert_eq!(load_mobile_settings(), MobileSettings::default());
            let s = MobileSettings {
                default_model: Some("claude-sonnet-4-6".into()),
                default_agent_id: None,
                plan_mode: true,
            };
            save_mobile_settings(&s).unwrap();
            assert_eq!(load_mobile_settings(), s);
        });
    }
}
