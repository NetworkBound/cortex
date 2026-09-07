//! Tauri commands for controlling Aider-style watch mode.
//!
//! # Wiring (NOT done in this commit — apply manually)
//!
//! 1. `src-tauri/src/lib.rs` add at the top of the module declarations:
//!    ```ignore
//!    pub mod watch_mode;
//!    ```
//!
//! 2. `src-tauri/src/commands/mod.rs` add:
//!    ```ignore
//!    pub mod watch_mode;
//!    ```
//!
//! 3. `src-tauri/src/lib.rs` register the commands inside the
//!    `tauri::generate_handler!` block:
//!    ```ignore
//!    commands::watch_mode::start_watch_mode,
//!    commands::watch_mode::stop_watch_mode,
//!    commands::watch_mode::is_watch_mode_active,
//!    ```
//!
//! The handle is stored in a process-wide `OnceCell<Mutex<Option<…>>>` rather
//! than in `AppState` to mirror the pattern used by
//! `commands::agui::start_agui_server` — this keeps the module self-contained
//! and avoids touching `app_state.rs`.

use std::{path::PathBuf, sync::Arc};

use once_cell::sync::OnceCell;
use parking_lot::Mutex;

use crate::watch_mode::{start_watch, WatchHandle};

/// Process-wide slot for the running watch handle. `None` ⇒ not running.
static WATCH_HANDLE: OnceCell<Arc<Mutex<Option<WatchHandle>>>> = OnceCell::new();

fn slot() -> Arc<Mutex<Option<WatchHandle>>> {
    WATCH_HANDLE
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Start (or restart) watch mode on `project_root`. If a previous watcher is
/// running, it is stopped first so this command is safe to call repeatedly
/// when the active project changes.
#[tauri::command]
pub async fn start_watch_mode(project_root: String, app: tauri::AppHandle) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!(
            "start_watch_mode: project_root is not a directory: {project_root}"
        ));
    }

    // Replace any existing handle. Drop releases the watcher cleanly.
    {
        let guard = slot();
        let mut locked = guard.lock();
        if let Some(handle) = locked.take() {
            handle.stop();
        }
    }

    let new_handle =
        start_watch(root, app).map_err(|e| format!("failed to start watch mode: {e:#}"))?;

    {
        let guard = slot();
        let mut locked = guard.lock();
        *locked = Some(new_handle);
    }

    Ok(())
}

/// Stop watch mode if running. Returns `true` if a watcher was stopped,
/// `false` if there was nothing to stop.
#[tauri::command]
pub async fn stop_watch_mode() -> Result<bool, String> {
    let guard = slot();
    let mut locked = guard.lock();
    if let Some(handle) = locked.take() {
        handle.stop();
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Report whether watch mode is currently running.
#[tauri::command]
pub async fn is_watch_mode_active() -> Result<bool, String> {
    let guard = slot();
    let locked = guard.lock();
    Ok(locked.is_some())
}
