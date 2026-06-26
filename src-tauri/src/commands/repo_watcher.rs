//! Tauri commands for the repo auto-sync watcher (ContextForge #15).
//!
//! Wraps `crate::repo_map::watcher` so the React frontend can start/stop
//! per-project filesystem watchers and query aggregate status. Events are
//! delivered via the `repo-watcher:event` window event (see
//! `RepoWatcherEvent`).

use std::path::PathBuf;

use crate::repo_map::watcher::{self, WatcherStatus};

fn validate_root(project_root: &str) -> Result<PathBuf, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is empty".into());
    }
    let p = PathBuf::from(project_root);
    if !p.exists() {
        return Err(format!("project_root does not exist: {project_root}"));
    }
    if !p.is_dir() {
        return Err(format!("project_root is not a directory: {project_root}"));
    }
    Ok(p)
}

/// Start (or restart) the repo watcher for `project_root`.
#[tauri::command]
pub async fn start_repo_watcher(
    project_root: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let root = validate_root(&project_root)?;
    watcher::start(root, app).map_err(|e| format!("start_repo_watcher: {e:#}"))
}

/// Stop the watcher for `project_root` if one is running. Returns `true` if
/// a watcher was actually stopped.
#[tauri::command]
pub async fn stop_repo_watcher(project_root: String) -> Result<bool, String> {
    let root = validate_root(&project_root)?;
    Ok(watcher::stop(root))
}

/// Snapshot of active watchers + aggregate change stats since the last
/// re-index. The frontend uses this to drive the StatusBar badge.
#[tauri::command]
pub async fn repo_watcher_status() -> Result<WatcherStatus, String> {
    Ok(watcher::status())
}

/// Reset the change counter for `project_root`. Called by the frontend
/// right after a successful re-index so the badge dismisses.
#[tauri::command]
pub async fn repo_watcher_reset(project_root: String) -> Result<(), String> {
    let root = validate_root(&project_root)?;
    watcher::reset_counter(&root);
    Ok(())
}
