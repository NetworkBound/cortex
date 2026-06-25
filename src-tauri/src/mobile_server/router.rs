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

/// Build the full mobile router around `state`.
pub fn build_router(state: MobileState) -> Router {
    // Loopback-only bind + `tailscale serve` in front are the real defense; CORS
    // here just lets the bundled SPA (same-origin) and local dev tooling call
    // the API from a browser.
    let cors = CorsLayer::new()
        .allow_origin(Any)
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
        .route("/api/ultimate", post(handlers::ultimate))
        .route("/api/approvals", get(handlers::list_approvals))
        .route("/api/approvals/{id}", post(handlers::resolve_approval))
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
