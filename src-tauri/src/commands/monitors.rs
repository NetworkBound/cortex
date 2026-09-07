//! Tauri commands for the background monitor feature. See
//! `src-tauri/src/monitors.rs` for the underlying lifecycle module.
//!
//! # Wiring
//!
//! 1. `src-tauri/src/commands/mod.rs`: `pub mod monitors;`
//! 2. `src-tauri/src/lib.rs` `invoke_handler!`:
//!    - `commands::monitors::start_monitors`
//!    - `commands::monitors::stop_monitors`
//!    - `commands::monitors::list_monitors`
//! 3. `src-tauri/src/lib.rs` module tree: `pub mod monitors;`
//! 4. App shutdown: call `crate::monitors::stop_all().await` from a
//!    `RunEvent::ExitRequested` or `RunEvent::Exit` handler.

use std::path::PathBuf;

use crate::monitors;

/// Read `<project_root>/.cortex/monitors/monitors.json` and start every
/// configured monitor. Replaces any previously running monitors. Returns the
/// names of the monitors that were successfully spawned.
#[tauri::command]
pub async fn start_monitors(
    project_root: String,
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!(
            "start_monitors: project_root is not a directory: {project_root}"
        ));
    }
    monitors::start_all(&root, app)
        .await
        .map_err(|e| format!("failed to start monitors: {e:#}"))
}

/// Kill every running monitor. Idempotent.
#[tauri::command]
pub async fn stop_monitors() -> Result<(), String> {
    monitors::stop_all().await;
    Ok(())
}

/// Return the parsed contents of `<project_root>/.cortex/monitors/monitors.json`
/// without starting anything. Missing config ⇒ empty list (not an error).
#[tauri::command]
pub async fn list_monitors(project_root: String) -> Result<Vec<monitors::MonitorSpec>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!(
            "list_monitors: project_root is not a directory: {project_root}"
        ));
    }
    monitors::load_specs(&root).map_err(|e| format!("failed to list monitors: {e:#}"))
}
