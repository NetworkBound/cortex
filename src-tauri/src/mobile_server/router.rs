//! axum `Router` for the mobile server. Mirrors [`crate::agui::server::router`]:
//! typed routes with `.with_state(...)`, a CORS layer, plus the mobile-specific
//! WebSocket route, identity middleware, response compression, and a single SPA
//! fallback (`ServeDir` + `ServeFile`).

use std::path::PathBuf;

use axum::{
    routing::{any, get, post},
    Router,
};
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
};

use super::{handlers, state::MobileState, ws};

/// Origins permitted cross-origin access to the mobile API. Any real website is
/// rejected by the browser's CORS check (blocking drive-by exfiltration). The
/// bundled SPA is served same-origin so it needs no entry. Mirrors
/// `agui::server::allowed_origins`. Non-browser clients (native app, CLI) aren't
/// subject to CORS at all; the loopback bind remains the primary defense.
fn mobile_allowed_origins() -> Vec<axum::http::HeaderValue> {
    [
        "tauri://localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
        "http://127.0.0.1:1420",
        "http://localhost:8788",
        "http://127.0.0.1:8788",
    ]
    .iter()
    .filter_map(|o| axum::http::HeaderValue::from_str(o).ok())
    .collect()
}

/// Build the full mobile router around `state`.
pub fn build_router(state: MobileState) -> Router {
    // Loopback bind + `tailscale serve` are the primary defense, but a wildcard
    // `Access-Control-Allow-Origin: *` would still let ANY website the user
    // visits read responses from these endpoints cross-origin — drive-by
    // exfiltration of chat/note snippets via /api/search, /api/brain, /api/sessions.
    // Restrict CORS to known local origins (the Tauri webview, the Vite dev
    // server, this server's own SPA) so browsers reject every real website.
    // Mirrors `agui::server::allowed_origins`. The bundled SPA is served
    // same-origin, so same-origin requests are unaffected.
    let cors = CorsLayer::new()
        .allow_origin(mobile_allowed_origins())
        .allow_methods(Any)
        .allow_headers(Any);

    // SINGLE fallback (multiple fallbacks panic): serve the mobile SPA's static
    // assets, falling back to its `index.html` for any unmatched path so SPA
    // deep links survive a hard reload. The dist dir is produced later by the
    // mobile SPA build; if it doesn't exist yet the static service simply 404s
    // while the API + WS routes keep working (the server never panics on a
    // missing dir — `ServeDir`/`ServeFile` resolve lazily per request).
    let dist = mobile_dist_dir();
    let index = dist.join("index.html");
    let spa = ServeDir::new(&dist).not_found_service(ServeFile::new(index));

    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/projects", get(handlers::projects))
        .route("/api/models", get(handlers::models))
        .route("/api/chat", post(handlers::chat))
        // OpenAI-compatible surface — desktop Model Fabric endpoints point here
        // to run this host's real CLI sessions (claude-cli/codex-cli) + Ollama.
        .route("/v1/models", get(handlers::v1_models))
        .route("/v1/chat/completions", post(handlers::v1_chat_completions))
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/sessions/:id/messages", get(handlers::session_messages))
        .route("/api/sessions/:id/export", post(handlers::export_session))
        .route("/api/search", get(handlers::search_chat))
        .route("/api/search/reindex", post(handlers::search_reindex))
        .route("/api/brain", post(handlers::brain))
        .route("/api/ultimate", post(handlers::ultimate))
        .route("/api/approvals", get(handlers::list_approvals))
        .route("/api/approvals/:id", post(handlers::resolve_approval))
        .route("/api/import/file", post(handlers::import_file))
        .route("/api/import/pull", post(handlers::import_pull))
        // `any(...)` not `get(...)` so the WS upgrade isn't method-gated.
        .route("/ws", any(ws::ws_handler))
        .fallback_service(spa)
        .layer(axum::middleware::from_fn(super::auth::identity))
        .layer(CompressionLayer::new())
        .layer(cors)
        .with_state(state)
}

/// Resolve the directory holding the built mobile SPA (`index.html` + assets).
///
/// Resolution order (first existing wins; falls back to the repo-relative path
/// even if missing so the SPA fallback has a stable, sensible target):
///   1. `CORTEX_MOBILE_DIST` env override (absolute path to a `dist/`).
///   2. `<repo>/mobile/dist` relative to this source file's crate dir.
///   3. `<cwd>/mobile/dist` (covers a headless run launched from the repo root).
fn mobile_dist_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CORTEX_MOBILE_DIST") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return p;
        }
    }
    // `CARGO_MANIFEST_DIR` is `<repo>/src-tauri`; the mobile SPA lives at
    // `<repo>/mobile/dist`.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo) = crate_dir.parent() {
        let candidate = repo.join("mobile").join("dist");
        if candidate.is_dir() {
            return candidate;
        }
    }
    // Last resort: cwd-relative (and returned even if absent, see fn docs).
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("mobile")
        .join("dist")
}
