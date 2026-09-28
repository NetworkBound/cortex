//! Cortex as an **MCP server** — `POST /mcp` on the mobile server.
//!
//! Implements the MCP Streamable HTTP transport in its simplest legal form:
//! one JSON-RPC 2.0 request per POST, one plain `application/json` response
//! (no SSE stream — every tool here returns quickly, so a single response is
//! exactly what the spec allows). External agents (Claude Code, Codex, Gemini
//! CLI) register it with `claude mcp add --transport http cortex <url>` and
//! then search the Brain, ask grounded questions, take/list/restore project
//! checkpoints and read run reliability — all without any data leaving the
//! machine.
//!
//! # Security
//!
//! - **Off by default.** `~/.cortex/mcp-server.json` (`{ enabled, allow_destructive }`)
//!   must be switched on from Settings; while off, `/mcp` answers 404 and does
//!   nothing else.
//! - **Bearer token, always.** Unlike the identity-only `Tailscale-User-Login`
//!   attribution on `/api/*`, every `/mcp` request must carry
//!   `Authorization: Bearer <token>` — even from loopback, because any local
//!   process (or a website via the browser) can reach 127.0.0.1. The token is a
//!   32-byte random hex string minted when the server is first enabled, stored
//!   in the encrypted key vault (OS keychain master key) and compared in
//!   constant time.
//! - **Origin check.** Browsers don't need CORS preflights for a
//!   `text/plain`-ish POST, so a cross-origin `Origin` header is rejected with
//!   the same rules as the WebSocket upgrade (`ws::origin_permitted`).
//! - **Destructive tools gated.** `checkpoint_restore` overwrites the working
//!   tree; it is only advertised/callable when `allow_destructive` is set.
//!   Checkpoint tools also refuse any `project_root` that is not a project
//!   Cortex itself discovers, so a token holder can't snapshot or restore
//!   arbitrary directories.

use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::commands::{brain_rag, chat_semantic, checkpoints, keyvault};
use crate::observability::tracing_store::TracingStore;

use super::state::MobileState;

/// Protocol revisions this server speaks. First entry is what we answer with
/// when a client asks for something we don't know.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

/// `serverInfo.name` in the initialize response.
pub const SERVER_NAME: &str = "cortex";

/// Key vault slot holding the bearer token (`provider` / `label`).
const VAULT_PROVIDER: &str = "cortex-mcp-server";
const VAULT_LABEL: &str = "bearer";

/// Token length in random bytes (hex-encoded → 64 chars).
const TOKEN_BYTES: usize = 32;

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

// ───────────────────────────────────────────────────────────────────────────
// Config (~/.cortex/mcp-server.json) + token (key vault)
// ───────────────────────────────────────────────────────────────────────────

/// Persisted server switches. Missing file / missing fields → everything off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Expose `checkpoint_restore` (overwrites the project tree).
    #[serde(default)]
    pub allow_destructive: bool,
}

fn config_path() -> Result<PathBuf, String> {
    crate::paths::cortex_dir()
        .map(|d| d.join("mcp-server.json"))
        .ok_or_else(|| "no home dir".to_string())
}

/// Load the config; any problem (missing, unreadable, corrupt) reads as the
/// all-off default so a broken file can never accidentally *enable* anything.
pub fn load_config() -> McpServerConfig {
    let Ok(path) = config_path() else {
        return McpServerConfig::default();
    };
    std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Persist the config as pretty JSON via a sibling temp file + rename.
pub fn save_config(cfg: &McpServerConfig) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| format!("serialize failed: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    Ok(())
}

/// Mint a fresh token: 32 OS-random bytes, lowercase hex.
pub fn generate_token() -> String {
    use aes_gcm::aead::rand_core::RngCore;
    let mut buf = [0u8; TOKEN_BYTES];
    aes_gcm::aead::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The stored bearer token, `Ok(None)` when none was minted yet. `Err` means
/// the vault itself is unreadable (locked keychain) — callers treat that as
/// "no valid token", never as "skip auth".
pub fn current_token() -> Result<Option<String>, String> {
    let entries = keyvault::lookup_provider_key_sync(VAULT_PROVIDER)?;
    Ok(entries.filter(|t| !t.is_empty()))
}

/// Generate + store a new token, returning it.
pub async fn rotate_token() -> Result<String, String> {
    let token = generate_token();
    keyvault::vault_set(
        VAULT_PROVIDER.to_string(),
        VAULT_LABEL.to_string(),
        token.clone(),
    )
    .await?;
    Ok(token)
}

// ───────────────────────────────────────────────────────────────────────────
// Auth gate
// ───────────────────────────────────────────────────────────────────────────

/// Constant-time byte equality (length is compared first, which leaks only
/// the length — the token length is public anyway).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract the token from an `Authorization: Bearer <token>` header value.
fn bearer_token(auth: &str) -> Option<&str> {
    let auth = auth.trim();
    let (scheme, rest) = auth.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let tok = rest.trim();
    (!tok.is_empty()).then_some(tok)
}

/// Decide whether a `/mcp` request may proceed. Pure so it can be unit-tested
/// without a listener. `expected` is the stored token (`None` = none minted /
/// vault unreadable → every request is refused while enabled).
///
/// Order matters: disabled → 404 (the endpoint "doesn't exist"), then Origin
/// (403, browser cross-site), then bearer (401).
pub fn gate(
    cfg: &McpServerConfig,
    expected: Option<&str>,
    authorization: Option<&str>,
    origin: Option<&str>,
    host: Option<&str>,
    forwarded_host: Option<&str>,
) -> Result<(), (StatusCode, &'static str)> {
    if !cfg.enabled {
        return Err((StatusCode::NOT_FOUND, "mcp server disabled"));
    }
    if !super::ws::origin_permitted(origin, host, forwarded_host) {
        return Err((StatusCode::FORBIDDEN, "origin not allowed"));
    }
    let Some(expected) = expected.filter(|t| !t.is_empty()) else {
        return Err((StatusCode::UNAUTHORIZED, "mcp server has no token"));
    };
    let Some(presented) = authorization.and_then(bearer_token) else {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token"));
    };
    if !ct_eq(presented.as_bytes(), expected.as_bytes()) {
        return Err((StatusCode::UNAUTHORIZED, "invalid bearer token"));
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// Handler
// ───────────────────────────────────────────────────────────────────────────

/// Everything a tool call needs, detached from axum so tests can build one
/// around `TracingStore::in_memory()`.
pub struct McpCtx {
    pub store: TracingStore,
    pub ollama_base: String,
    /// Configured generation model (`brain_answer` default).
    pub chat_model: String,
    pub vault: Option<PathBuf>,
    pub allow_destructive: bool,
    /// Project roots checkpoint tools may touch. `None` → discover at call
    /// time via `projects::discover_projects` (the app's own roster).
    pub allowed_roots: Option<Vec<PathBuf>>,
}

impl McpCtx {
    fn from_state(state: &MobileState, allow_destructive: bool) -> Self {
        let cfg = state.app.config.read();
        Self {
            store: state.store.clone(),
            ollama_base: cfg.ollama_base_url.clone(),
            chat_model: cfg.ollama_model.clone(),
            vault: cfg.obsidian_vault.clone(),
            allow_destructive,
            allowed_roots: None,
        }
    }

    /// Validate a caller-supplied `project_root`: must be an existing directory
    /// that is one of Cortex's known projects. Returns the canonical path.
    fn resolve_root(&self, raw: &str) -> Result<PathBuf, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("project_root is required".into());
        }
        let path = PathBuf::from(raw);
        if !path.is_dir() {
            return Err(format!("not a directory: {raw}"));
        }
        let canon = crate::paths::canonicalize_lossy(&path);
        let allowed: Vec<PathBuf> = match &self.allowed_roots {
            Some(v) => v.clone(),
            None => crate::projects::discover_projects(self.vault.clone())
                .into_iter()
                .map(|p| p.root)
                .collect(),
        };
        if allowed
            .iter()
            .any(|r| crate::paths::canonicalize_lossy(r) == canon)
        {
            Ok(canon)
        } else {
            Err(format!(
                "project_root is not a registered Cortex project: {raw}"
            ))
        }
    }
}

/// `POST /mcp`.
pub async fn handle(State(state): State<MobileState>, headers: HeaderMap, body: Bytes) -> Response {
    let cfg = load_config();
    let hdr = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    // Only touch the vault when enabled: a disabled server must be inert.
    let expected = if cfg.enabled {
        match current_token() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "mcp server: token unavailable");
                None
            }
        }
    } else {
        None
    };
    if let Err((status, msg)) = gate(
        &cfg,
        expected.as_deref(),
        hdr("authorization"),
        hdr("origin"),
        hdr("host"),
        hdr("x-forwarded-host"),
    ) {
        if status != StatusCode::NOT_FOUND {
            tracing::warn!(status = %status, reason = msg, "mcp server: request refused");
        }
        let mut resp = (status, Json(json!({ "error": msg }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"cortex\""),
            );
        }
        return resp;
    }
    let ctx = McpCtx::from_state(&state, cfg.allow_destructive);
    match handle_body(&ctx, &body).await {
        Some(v) => Json(v).into_response(),
        // Notification: accepted, nothing to say.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// JSON-RPC dispatch
// ───────────────────────────────────────────────────────────────────────────

/// A JSON-RPC error (protocol level — distinct from a tool's `isError` result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, msg)
    }
}

fn error_response(id: Value, err: RpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": err.code, "message": err.message },
    })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Parse one POST body and produce the response body, or `None` for a
/// notification (→ 202 with no body).
pub async fn handle_body(ctx: &McpCtx, body: &[u8]) -> Option<Value> {
    let req: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                RpcError::new(PARSE_ERROR, format!("parse error: {e}")),
            ))
        }
    };
    Some(handle_message(ctx, req).await?)
}

/// Dispatch one already-parsed JSON-RPC message.
pub async fn handle_message(ctx: &McpCtx, req: Value) -> Option<Value> {
    let Some(obj) = req.as_object() else {
        // Batches were removed in 2025-06-18; we never supported them.
        return Some(error_response(
            Value::Null,
            RpcError::new(INVALID_REQUEST, "expected a single JSON-RPC request object"),
        ));
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = id.is_null();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        if is_notification {
            return None;
        }
        return Some(error_response(
            id,
            RpcError::new(INVALID_REQUEST, "missing method"),
        ));
    };
    if is_notification {
        // `notifications/initialized`, `notifications/cancelled`, … — nothing
        // to do for a stateless server.
        return None;
    }
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(match dispatch(ctx, method, &params).await {
        Ok(result) => ok_response(id, result),
        Err(err) => error_response(id, err),
    })
}

async fn dispatch(ctx: &McpCtx, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize_result(
            params.get("protocolVersion").and_then(Value::as_str),
        )),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_specs(ctx.allow_destructive) })),
        "tools/call" => call_tool(ctx, params).await,
        _ => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )),
    }
}

/// Echo a supported protocol version, else offer our newest.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

fn initialize_result(requested: Option<&str>) -> Value {
    json!({
        "protocolVersion": negotiate_protocol_version(requested),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Cortex desktop: local Brain (semantic memory over chats + Obsidian notes), \
                         project checkpoints and run reliability. Everything stays on this machine.",
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Tools
// ───────────────────────────────────────────────────────────────────────────

/// Tool names that overwrite user data; only listed when `allow_destructive`.
const DESTRUCTIVE_TOOLS: &[&str] = &["checkpoint_restore"];

/// The advertised tool list (MCP `tools/list` shape).
pub fn tool_specs(allow_destructive: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "brain_search",
            "description": "Semantic search over the Cortex Brain: past chat sessions and (optionally) indexed Obsidian/memory notes. Returns ranked snippets with session ids / note paths.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language query." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "include_notes": { "type": "boolean", "default": true, "description": "Include vault/memory notes, not just chats." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "brain_answer",
            "description": "Ask the Brain a question and get a grounded answer with numbered citations (RAG over chats + notes via the local Ollama model).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 12, "default": 8, "description": "Max citations to retrieve." },
                    "model": { "type": "string", "description": "Override the Ollama generation model." },
                    "project_root": { "type": "string", "description": "Scope retrieval to this project directory." }
                },
                "required": ["question"]
            }
        }),
        json!({
            "name": "recent_sessions",
            "description": "List recent Cortex chat sessions (id, title, last activity, message count, preview).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 }
                }
            }
        }),
        json!({
            "name": "reliability_summary",
            "description": "Aggregate run reliability (success/error rates, latency, tokens) by provider and model, plus MCP tool-call stats.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since_hours": { "type": "integer", "minimum": 1, "description": "Only runs started within the last N hours (default: all)." }
                }
            }
        }),
        json!({
            "name": "checkpoint_list",
            "description": "List Cortex checkpoints (tar.gz snapshots under .cortex/checkpoints) for a registered project, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string", "description": "Absolute path of a project Cortex knows about." }
                },
                "required": ["project_root"]
            }
        }),
        json!({
            "name": "checkpoint_create",
            "description": "Snapshot a registered project's working tree into a new Cortex checkpoint (max 50MB).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "label": { "type": "string", "description": "Optional human label." }
                },
                "required": ["project_root"]
            }
        }),
    ];
    if allow_destructive {
        tools.push(json!({
            "name": "checkpoint_restore",
            "description": "DESTRUCTIVE: overwrite a registered project's working tree with a checkpoint. Refuses when the tree has uncommitted changes unless force=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "id": { "type": "string", "description": "Checkpoint id from checkpoint_list." },
                    "force": { "type": "boolean", "default": false }
                },
                "required": ["project_root", "id"]
            }
        }));
    }
    tools
}

/// Is `name` a tool this server exposes under the current gating?
pub fn tool_available(name: &str, allow_destructive: bool) -> bool {
    if DESTRUCTIVE_TOOLS.contains(&name) && !allow_destructive {
        return false;
    }
    tool_specs(true)
        .iter()
        .any(|t| t.get("name").and_then(Value::as_str) == Some(name))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn arg_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn arg_usize(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default)
        .clamp(1, max)
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

async fn call_tool(ctx: &McpCtx, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("missing tool name"))?;
    if !tool_available(name, ctx.allow_destructive) {
        return Err(RpcError::invalid_params(format!("unknown tool: {name}")));
    }
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err(RpcError::invalid_params("arguments must be an object"));
    }
    Ok(match run_tool(ctx, name, &args).await {
        Ok(v) => {
            let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": v,
                "isError": false,
            })
        }
        Err(msg) => json!({
            "content": [{ "type": "text", "text": msg }],
            "isError": true,
        }),
    })
}

fn to_json<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Execute one tool. `Err` becomes an `isError` tool result (the model sees
/// the message), never a protocol error.
async fn run_tool(ctx: &McpCtx, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "brain_search" => {
            let query = arg_str(args, "query")?;
            let limit = arg_usize(args, "limit", 10, 50);
            let include_notes = arg_bool(args, "include_notes", true);
            let model = crate::memory::embed::embed_model();
            let hits = chat_semantic::search(
                &ctx.store,
                &ctx.ollama_base,
                &model,
                query,
                limit,
                include_notes,
            )
            .await?;
            to_json(hits)
        }
        "brain_answer" => {
            let question = arg_str(args, "question")?;
            let limit = arg_usize(args, "limit", 8, 12);
            let model =
                brain_rag::resolve_chat_model(arg_opt_str(args, "model"), ctx.chat_model.clone());
            let project_root = arg_opt_str(args, "project_root").map(PathBuf::from);
            let ans = brain_rag::brain_answer(
                &ctx.store,
                &ctx.ollama_base,
                &model,
                ctx.vault.clone(),
                project_root,
                question,
                limit,
            )
            .await?;
            to_json(ans)
        }
        "recent_sessions" => {
            let limit = arg_usize(args, "limit", 20, 200);
            let rows = ctx
                .store
                .recent_chat_sessions(limit)
                .map_err(|e| e.to_string())?;
            to_json(rows)
        }
        "reliability_summary" => {
            let since_ms = args
                .get("since_hours")
                .and_then(Value::as_u64)
                .filter(|h| *h > 0)
                .map(|h| chrono::Utc::now().timestamp_millis() - (h as i64) * 3_600_000);
            let report = ctx
                .store
                .reliability_summary(since_ms)
                .map_err(|e| e.to_string())?;
            to_json(report)
        }
        "checkpoint_list" => {
            let root = ctx.resolve_root(arg_str(args, "project_root")?)?;
            let list = blocking(move || checkpoints::list_checkpoints_sync(&root)).await?;
            to_json(list)
        }
        "checkpoint_create" => {
            let root = ctx.resolve_root(arg_str(args, "project_root")?)?;
            let label = arg_opt_str(args, "label");
            let info = blocking(move || checkpoints::make_checkpoint(&root, label)).await?;
            to_json(info)
        }
        "checkpoint_restore" => {
            // Belt and braces: `tool_available` already refused this when
            // destructive tools are off.
            if !ctx.allow_destructive {
                return Err("destructive tools are disabled in Cortex settings".into());
            }
            let root = ctx.resolve_root(arg_str(args, "project_root")?)?;
            let id = arg_str(args, "id")?.to_string();
            let force = arg_bool(args, "force", false);
            let restored = id.clone();
            blocking(move || checkpoints::restore_checkpoint_core(&root, &id, force)).await?;
            Ok(json!({ "restored": restored, "force": force }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// Run blocking checkpoint IO off the async executor.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("join error: {e}"))?
}

// ───────────────────────────────────────────────────────────────────────────
// Client snippets (Settings UI)
// ───────────────────────────────────────────────────────────────────────────

/// Ready-to-paste client configuration for the common MCP hosts.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClientSnippets {
    pub url: String,
    pub token: String,
    /// `claude mcp add …` one-liner.
    pub claude_code: String,
    /// Block for `~/.codex/config.toml`.
    pub codex_toml: String,
    /// Block for `~/.gemini/settings.json` (`mcpServers` entry).
    pub gemini_json: String,
}

/// The `/mcp` URL for the mobile server's bind address.
pub fn server_url() -> String {
    let addr = super::resolve_bind();
    format!("http://{addr}/mcp")
}

pub fn client_snippets(url: &str, token: &str) -> ClientSnippets {
    let claude_code = format!(
        "claude mcp add --transport http cortex {url} --header \"Authorization: Bearer {token}\""
    );
    let codex_toml = format!(
        "[mcp_servers.cortex]\nurl = \"{url}\"\nhttp_headers = {{ \"Authorization\" = \"Bearer {token}\" }}\n"
    );
    let gemini_json = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "cortex": {
                "httpUrl": url,
                "headers": { "Authorization": format!("Bearer {token}") }
            }
        }
    }))
    .unwrap_or_default();
    ClientSnippets {
        url: url.to_string(),
        token: token.to_string(),
        claude_code,
        codex_toml,
        gemini_json,
    }
}

/// Mask a token for display: first 6 + last 4 chars (or all bullets when short).
pub fn mask_token(token: &str) -> String {
    let n = token.chars().count();
    if n <= 12 {
        return "•".repeat(n.max(1));
    }
    let head: String = token.chars().take(6).collect();
    let tail: String = token.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(allow_destructive: bool, roots: Vec<PathBuf>) -> McpCtx {
        McpCtx {
            store: TracingStore::in_memory(),
            // Unreachable on purpose: no test here may hit the network.
            ollama_base: "http://127.0.0.1:1".into(),
            chat_model: String::new(),
            vault: None,
            allow_destructive,
            allowed_roots: Some(roots),
        }
    }

    fn rpc(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    async fn call(ctx: &McpCtx, id: i64, method: &str, params: Value) -> Value {
        handle_message(ctx, rpc(id, method, params))
            .await
            .expect("request gets a response")
    }

    fn tool_names(resp: &Value) -> Vec<String> {
        resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    // ── config ──────────────────────────────────────────────────────────

    #[test]
    fn config_roundtrip_and_defaults() {
        crate::paths::test_home::with_temp_home(|_| {
            assert_eq!(load_config(), McpServerConfig::default());
            let cfg = McpServerConfig {
                enabled: true,
                allow_destructive: false,
            };
            save_config(&cfg).unwrap();
            assert_eq!(load_config(), cfg);
            // Corrupt file reads as all-off, never as enabled.
            std::fs::write(config_path().unwrap(), b"{ not json").unwrap();
            assert_eq!(load_config(), McpServerConfig::default());
        });
    }

    #[test]
    fn missing_fields_default_off() {
        let cfg: McpServerConfig = serde_json::from_str("{}").unwrap();
        assert!(!cfg.enabled && !cfg.allow_destructive);
        let cfg: McpServerConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(cfg.enabled && !cfg.allow_destructive);
    }

    // ── auth ────────────────────────────────────────────────────────────

    #[test]
    fn token_is_64_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), TOKEN_BYTES * 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn ct_eq_semantics() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer   abc "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None);
        assert_eq!(bearer_token("Bearer "), None);
    }

    #[test]
    fn gate_disabled_is_404_regardless_of_token() {
        let cfg = McpServerConfig::default();
        let r = gate(&cfg, Some("tok"), Some("Bearer tok"), None, None, None);
        assert_eq!(r.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn gate_requires_matching_bearer() {
        let cfg = McpServerConfig {
            enabled: true,
            allow_destructive: false,
        };
        let host = Some("127.0.0.1:8788");
        assert_eq!(
            gate(&cfg, Some("tok"), None, None, host, None)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            gate(&cfg, Some("tok"), Some("Bearer nope"), None, host, None)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            gate(&cfg, Some("tok"), Some("Basic tok"), None, host, None)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        // No token minted / vault unreadable → refuse, never bypass.
        assert_eq!(
            gate(&cfg, None, Some("Bearer tok"), None, host, None)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert!(gate(&cfg, Some("tok"), Some("Bearer tok"), None, host, None).is_ok());
    }

    #[test]
    fn gate_rejects_cross_origin_browser_requests() {
        let cfg = McpServerConfig {
            enabled: true,
            allow_destructive: false,
        };
        let host = Some("127.0.0.1:8788");
        let r = gate(
            &cfg,
            Some("tok"),
            Some("Bearer tok"),
            Some("https://evil.example"),
            host,
            None,
        );
        assert_eq!(r.unwrap_err().0, StatusCode::FORBIDDEN);
        // Same-origin SPA and no-Origin native clients pass.
        assert!(gate(
            &cfg,
            Some("tok"),
            Some("Bearer tok"),
            Some("http://127.0.0.1:8788"),
            host,
            None
        )
        .is_ok());
    }

    // ── JSON-RPC dispatch ───────────────────────────────────────────────

    #[tokio::test]
    async fn initialize_negotiates_protocol_version() {
        let c = ctx(false, vec![]);
        let r = call(
            &c,
            1,
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }),
        )
        .await;
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "cortex");
        assert_eq!(
            r["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert!(r["result"]["capabilities"]["tools"].is_object());

        let r = call(
            &c,
            2,
            "initialize",
            json!({ "protocolVersion": "1999-01-01" }),
        )
        .await;
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let r = call(&c, 3, "initialize", json!({})).await;
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let c = ctx(false, vec![]);
        let r = handle_message(
            &c,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn ping_and_unknown_method() {
        let c = ctx(false, vec![]);
        let r = call(&c, 7, "ping", Value::Null).await;
        assert_eq!(r["result"], json!({}));
        let r = call(&c, 8, "resources/list", Value::Null).await;
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(r["id"], 8);
    }

    #[tokio::test]
    async fn parse_error_and_batch_rejected() {
        let c = ctx(false, vec![]);
        let r = handle_body(&c, b"{ nope").await.unwrap();
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert!(r["id"].is_null());
        let r = handle_body(&c, br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        let r = handle_body(&c, br#"{"jsonrpc":"2.0","id":5}"#)
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        assert_eq!(r["id"], 5);
    }

    #[tokio::test]
    async fn tools_list_gates_destructive_tools() {
        let r = call(&ctx(false, vec![]), 1, "tools/list", json!({})).await;
        let names = tool_names(&r);
        for n in [
            "brain_search",
            "brain_answer",
            "recent_sessions",
            "reliability_summary",
            "checkpoint_list",
            "checkpoint_create",
        ] {
            assert!(names.contains(&n.to_string()), "missing {n}");
        }
        assert!(!names.contains(&"checkpoint_restore".to_string()));
        // Every tool has an object inputSchema.
        for t in r["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
        }

        let r = call(&ctx(true, vec![]), 1, "tools/list", json!({})).await;
        assert!(tool_names(&r).contains(&"checkpoint_restore".to_string()));
    }

    #[tokio::test]
    async fn tools_call_bad_params() {
        let c = ctx(false, vec![]);
        let r = call(&c, 1, "tools/call", json!({})).await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = call(&c, 2, "tools/call", json!({ "name": "nope" })).await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = call(
            &c,
            3,
            "tools/call",
            json!({ "name": "recent_sessions", "arguments": 5 }),
        )
        .await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        // Destructive tool while gated off → treated as unknown (protocol error).
        let r = call(
            &c,
            4,
            "tools/call",
            json!({ "name": "checkpoint_restore", "arguments": { "project_root": "/x", "id": "y" } }),
        )
        .await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn missing_required_argument_is_tool_error_not_protocol_error() {
        let c = ctx(false, vec![]);
        let r = call(
            &c,
            1,
            "tools/call",
            json!({ "name": "brain_search", "arguments": {} }),
        )
        .await;
        assert!(r.get("error").is_none());
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("query"));
    }

    #[tokio::test]
    async fn read_only_store_tools_work_on_empty_store() {
        let c = ctx(false, vec![]);
        let r = call(
            &c,
            1,
            "tools/call",
            json!({ "name": "recent_sessions", "arguments": { "limit": 5 } }),
        )
        .await;
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["structuredContent"], json!([]));
        let r = call(
            &c,
            2,
            "tools/call",
            json!({ "name": "reliability_summary", "arguments": { "since_hours": 24 } }),
        )
        .await;
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["structuredContent"]["totals"].is_object());
    }

    #[tokio::test]
    async fn checkpoint_tools_refuse_unregistered_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(true, vec![]); // nothing registered
        let r = call(
            &c,
            1,
            "tools/call",
            json!({ "name": "checkpoint_list", "arguments": { "project_root": tmp.path().to_string_lossy() } }),
        )
        .await;
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not a registered"));
    }

    #[tokio::test]
    async fn checkpoint_create_list_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let file = root.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        let c = ctx(true, vec![root.clone()]);
        let root_s = root.to_string_lossy().to_string();

        let r = call(
            &c,
            1,
            "tools/call",
            json!({ "name": "checkpoint_create", "arguments": { "project_root": root_s, "label": "mcp" } }),
        )
        .await;
        assert_eq!(r["result"]["isError"], false, "{r}");
        let id = r["result"]["structuredContent"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(r["result"]["structuredContent"]["label"], "mcp");

        let r = call(
            &c,
            2,
            "tools/call",
            json!({ "name": "checkpoint_list", "arguments": { "project_root": root_s } }),
        )
        .await;
        assert_eq!(r["result"]["structuredContent"][0]["id"], id);

        std::fs::write(&file, "v2").unwrap();
        let r = call(
            &c,
            3,
            "tools/call",
            json!({ "name": "checkpoint_restore", "arguments": { "project_root": root_s, "id": id, "force": true } }),
        )
        .await;
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
    }

    // ── snippets ────────────────────────────────────────────────────────

    #[test]
    fn snippets_embed_url_and_token() {
        let s = client_snippets("http://127.0.0.1:8788/mcp", "abc123");
        assert!(s
            .claude_code
            .starts_with("claude mcp add --transport http cortex http://127.0.0.1:8788/mcp"));
        assert!(s.claude_code.contains("Authorization: Bearer abc123"));
        assert!(s.codex_toml.contains("[mcp_servers.cortex]"));
        assert!(s.codex_toml.contains("url = \"http://127.0.0.1:8788/mcp\""));
        let g: Value = serde_json::from_str(&s.gemini_json).unwrap();
        assert_eq!(
            g["mcpServers"]["cortex"]["httpUrl"],
            "http://127.0.0.1:8788/mcp"
        );
        assert_eq!(
            g["mcpServers"]["cortex"]["headers"]["Authorization"],
            "Bearer abc123"
        );
    }

    #[test]
    fn mask_token_keeps_ends_only() {
        assert_eq!(mask_token("short"), "•••••");
        let m = mask_token("0123456789abcdef0123456789abcdef");
        assert_eq!(m, "012345…cdef");
    }
}
