//! Tauri commands for the localhost dev-server preview pane.
//!
//! Thin shim around `crate::preview` — the heavy lifting (port sniffing,
//! `<title>` parsing, event emission) lives in that module. These commands
//! exist so the React frontend can drive the watcher and trigger one-shot
//! sweeps.

use crate::preview::{self, DetectedServer};

/// Run a single synchronous sweep of the probe ports. Useful when the
/// preview tab first opens — the watcher emits *changes* only, so without
/// this the UI would have to wait up to 3s for the first event.
#[tauri::command]
pub async fn list_dev_servers() -> Result<Vec<DetectedServer>, String> {
    Ok(preview::sweep_once().await)
}

/// Start the background poll loop. Safe to call repeatedly — any existing
/// watcher is replaced.
#[tauri::command]
pub async fn start_preview_watcher(app: tauri::AppHandle) -> Result<(), String> {
    preview::start(app).map_err(|e| format!("start_preview_watcher: {e:#}"))
}

/// Stop the background poll loop if running. Returns `true` if a watcher
/// was actually stopped.
#[tauri::command]
pub async fn stop_preview_watcher() -> Result<bool, String> {
    Ok(preview::stop())
}
