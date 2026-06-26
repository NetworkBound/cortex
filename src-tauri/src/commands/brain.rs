use crate::app_state::AppState;
use crate::brain::{build_snapshot, BrainSnapshot};
use crate::observability::tracing_store::TracingStore;
use std::path::PathBuf;
use tauri::State;

#[tauri::command]
pub async fn brain_snapshot(
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<BrainSnapshot, String> {
    let vault = state.config.read().obsidian_vault.clone();
    Ok(build_snapshot(&store, vault.as_deref()))
}

#[tauri::command]
pub async fn set_obsidian_vault(path: Option<String>, state: State<'_, AppState>) -> Result<(), String> {
    state.config.write().obsidian_vault = path.map(PathBuf::from);
    Ok(())
}
