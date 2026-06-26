//! Embedded HTTP + WebSocket server that serves the Cortex **mobile** web
//! client and bridges it to the existing backend (the same agent registry +
//! orchestrator the desktop Tauri app drives).
//!
//! This mirrors the existing [`crate::agui::server`] axum server in every
//! structural way — router build, `tokio::net::TcpListener` bind, spawned onto
//! Tauri's tokio runtime via `tauri::async_runtime::spawn`, reaching the agent
//! registry through a cloned [`AppState`] — but exposes a Cortex-native JSON +
//! WebSocket surface designed for a small mobile SPA rather than the AG-UI wire
//! protocol.
//!
//! # Wiring
//!
//! - `src-tauri/src/lib.rs` declares `pub mod mobile_server;` and, from the
//!   `.setup(...)` hook, calls [`spawn`] with the managed [`AppState`] and
//!   [`TracingStore`] (the latter is what `run_ultimate` needs).
//! - A headless entrypoint ([`serve_blocking`]) lets the same server run on a
//!   VM with no GUI — see `src/bin/cortex-serve.rs`.
//!
//! # Bind + ports
//!
//! Binds **`127.0.0.1`** only (loopback). The port defaults to `8788` and is
//! overridable via the `CORTEX_MOBILE_PORT` env var.
//!
//! # Security model
//!
//! The server performs no real authentication itself. It is meant to sit behind
//! `tailscale serve`, which terminates TLS and injects a `Tailscale-User-Login`
//! header identifying the authenticated tailnet user. The [`auth`] middleware
//! reads that header and attaches it as the request identity when present, but
//! does **not** hard-fail when it is absent — that would break local dev where
//! the server is reached directly over loopback. Security therefore rests on the
//! `127.0.0.1` bind plus the `tailscale serve` proxy in front; never expose this
//! port directly to a public interface.
//!
//! # Endpoints
//!
//! - `GET  /api/health`              → `{ ok, version }`
//! - `GET  /api/projects`            → `[ProjectMeta]`
//! - `GET  /api/models`             → `[String]` (connected-model roster)
//! - `POST /api/chat`               → start a chat run; streams over `/ws`
//! - `POST /api/ultimate`           → run the ultimate agent; streams over `/ws`
//! - `GET  /api/approvals`          → pending approvals
//! - `POST /api/approvals/{id}`     → resolve an approval
//! - `GET  /ws`                     → WebSocket fan-out of streaming events
//! - everything else                → the mobile SPA (`ServeDir` + SPA fallback)

pub mod auth;
pub mod state;
pub mod ws;

mod handlers;
mod router;

use std::net::SocketAddr;

use crate::app_state::AppState;
use crate::observability::tracing_store::TracingStore;

pub use router::build_router;
pub use state::{MobileEvent, MobileState};

/// Default loopback port for the mobile server. Overridable via
/// `CORTEX_MOBILE_PORT`.
pub const DEFAULT_PORT: u16 = 8788;

/// Resolve the bind address: `127.0.0.1:<CORTEX_MOBILE_PORT|8788>`.
pub fn resolve_bind() -> SocketAddr {
    let port = std::env::var("CORTEX_MOBILE_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Spawn the mobile server onto the ambient (Tauri) tokio runtime. Non-blocking:
/// returns once the listener is bound. Errors binding are logged and swallowed
/// so a port clash can never take down app startup.
///
/// `app` is the shared [`AppState`] (agent registry + config). `store` is the
/// [`TracingStore`] the ultimate orchestrator records into. Both are cheap
/// `Clone`s of `Arc`-backed handles.
pub async fn spawn(app: AppState, store: TracingStore) -> anyhow::Result<SocketAddr> {
    let addr = resolve_bind();
    let state = MobileState::new(app, store);
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router.into_make_service()).await {
            tracing::error!(error = %e, "mobile server exited with error");
        }
    });

    tracing::info!(addr = %bound, "mobile server listening");
    Ok(bound)
}

/// Headless entrypoint: construct the server and serve it **blocking** on the
/// current tokio runtime until the process is killed. Used by the
/// `cortex-serve` binary so the same HTTP/WS surface can run on a GUI-less VM.
pub async fn serve_blocking(app: AppState, store: TracingStore) -> anyhow::Result<()> {
    let addr = resolve_bind();
    let state = MobileState::new(app, store);
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);
    tracing::info!(addr = %bound, "mobile server (headless) listening");

    axum::serve(listener, router.into_make_service()).await?;
    Ok(())
}
