//! Tauri command surface for memory snapshots. All heavy lifting lives in
//! `crate::memory::snapshots`; this file just adapts paths from the frontend
//! and runs the blocking IO inside `spawn_blocking`.

use crate::memory::snapshots::{self, RollbackReport, SnapshotMeta};
use std::path::PathBuf;

/// Capture every memory source into a new gzipped tarball.
#[tauri::command]
pub async fn create_snapshot(
    label: String,
    active_project: Option<String>,
) -> Result<SnapshotMeta, String> {
    tokio::task::spawn_blocking(move || {
        let proj = active_project.map(PathBuf::from);
        snapshots::create(&label, proj.as_deref())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

/// List existing snapshots, newest first.
#[tauri::command]
pub async fn list_snapshots() -> Result<Vec<SnapshotMeta>, String> {
    tokio::task::spawn_blocking(snapshots::list)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// Restore a snapshot; never overwrites files newer than the snapshot itself
/// and never writes outside the original capture roots.
#[tauri::command]
pub async fn rollback_snapshot(id: String) -> Result<RollbackReport, String> {
    tokio::task::spawn_blocking(move || snapshots::rollback(&id))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn delete_snapshot(id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || snapshots::delete(&id))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// Keep the newest `keep` snapshots; delete the rest. Returns count deleted.
#[tauri::command]
pub async fn prune_snapshots(keep: usize) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || snapshots::prune(keep))
        .await
        .map_err(|e| format!("join: {e}"))?
}
