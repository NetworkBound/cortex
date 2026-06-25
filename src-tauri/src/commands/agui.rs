//! Tauri commands for controlling the embedded AG-UI Protocol server.
//!
//! # Wiring
//!
//! 1. `src-tauri/src/commands/mod.rs` must add:
//!    ```ignore
//!    pub mod agui;
//!    ```
//!
//! 2. `src-tauri/src/lib.rs` `invoke_handler!` macro must add:
//!    ```ignore
//!    commands::agui::start_agui_server,
//!    commands::agui::stop_agui_server,
//!    ```
//!
//! 3. `src-tauri/src/lib.rs` must also expose the module tree:
//!    ```ignore
//!    pub mod agui;
//!    ```
//!
//! The handle is currently stored in a process-wide `OnceCell` rather than in
//! `AppState`, so this module is self-contained and does not require any
//! changes to `app_state.rs`. A follow-up can lift the handle into
//! `AppState` once we want to surface server status / port via existing
//! state-snapshot commands.

use std::{net::SocketAddr, sync::Arc};

use once_cell::sync::OnceCell;
use parking_lot::Mutex;

use crate::agui::{self, server::AguiServerHandle};
use crate::app_state::AppState;

/// Process-wide slot for the running server handle. `None` ⇒ not running.
static AGUI_HANDLE: OnceCell<Arc<Mutex<Option<AguiServerHandle>>>> = OnceCell::new();

fn slot() -> Arc<Mutex<Option<AguiServerHandle>>> {
    AGUI_HANDLE
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Start the AG-UI Protocol server.
///
/// `bind` is optional; when omitted the server binds to
/// [`agui::DEFAULT_BIND`] (`127.0.0.1:8643`). Returns the bound address as a
/// string so the frontend can display the actual port (useful when the caller
/// passed `127.0.0.1:0` for an ephemeral port).
///
/// Idempotent: if a server is already running this returns its existing
/// address without spawning a second one.
#[tauri::command]
pub async fn start_agui_server(
    bind: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    {
        let guard = slot();
        let locked = guard.lock();
        if let Some(handle) = locked.as_ref() {
            if handle.is_running() {
                return Ok(handle.addr().to_string());
            }
        }
    }

    let parsed: Option<SocketAddr> = match bind {
        Some(s) if !s.is_empty() => Some(
            s.parse::<SocketAddr>()
                .map_err(|e| format!("invalid bind address {s:?}: {e}"))?,
        ),
        _ => None,
    };

    let app = state.inner().clone();
    let handle = agui::server::spawn(parsed, app)
        .await
        .map_err(|e| format!("failed to start agui server: {e}"))?;

    let addr = handle.addr().to_string();
    {
        let guard = slot();
        let mut locked = guard.lock();
        *locked = Some(handle);
    }
    Ok(addr)
}

/// Stop the AG-UI Protocol server if it's running. No-op if it isn't.
///
/// Returns `true` if a running server was stopped, `false` if there was
/// nothing to stop.
#[tauri::command]
pub async fn stop_agui_server() -> Result<bool, String> {
    let guard = slot();
    let mut locked = guard.lock();
    if let Some(handle) = locked.take() {
        let was_running = handle.is_running();
        handle.stop();
        Ok(was_running)
    } else {
        Ok(false)
    }
}
