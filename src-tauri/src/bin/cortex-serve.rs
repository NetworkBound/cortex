//! Headless Cortex server — runs ONLY the embedded mobile HTTP/WebSocket
//! server (no GUI, no Tauri window). Intended to run on a VM behind
//! `tailscale serve`.
//!
//! It constructs the minimal backend state (agent registry + tracing store)
//! via [`cortex_lib::build_headless_state`] — the same adapters the desktop app
//! registers — then serves the mobile API/WS surface, blocking until killed.
//!
//! Bind: `127.0.0.1:8788` (override via `CORTEX_MOBILE_PORT`). Static SPA dir:
//! `<repo>/mobile/dist` (override via `CORTEX_MOBILE_DIST`).
//!
//! Run with:
//!     cargo run --bin cortex-serve
//! or the built binary directly.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cortex_lib::init_tracing();

    let (state, store) = cortex_lib::build_headless_state();

    tracing::info!("cortex-serve: starting headless mobile server");
    cortex_lib::mobile_server::serve_blocking(state, store).await?;
    Ok(())
}
