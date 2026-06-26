// WIRING (do in lib.rs and commands/mod.rs):
//   1. In src-tauri/src/lib.rs (top-level module list):
//        pub mod repo_map;
//   2. In src-tauri/src/commands/mod.rs:
//        pub mod repo_map;
//   3. In the tauri::Builder::default()...invoke_handler in lib.rs, add:
//        commands::repo_map::repo_map,
//        commands::repo_map::repo_map_text,
//
// These commands expose an Aider-style symbol map of the active project so the
// The gateway agent always has structural context without manual @-mentions.

use std::path::PathBuf;

use crate::repo_map::{compute_repo_map, format_as_text, repo_symbols as repo_symbols_impl, RepoMap, SymbolHit};

/// Default cap on number of files included in a repo map.
const DEFAULT_MAX_FILES: usize = 200;
/// Default cap on number of symbol hits returned to the @-picker.
const DEFAULT_SYMBOL_LIMIT: usize = 50;

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

/// Returns the full structured repo map for `project_root`.
#[tauri::command]
pub async fn repo_map(project_root: String) -> Result<RepoMap, String> {
    let root = validate_root(&project_root)?;
    let map = tokio::task::spawn_blocking(move || compute_repo_map(&root, DEFAULT_MAX_FILES))
        .await
        .map_err(|e| format!("repo_map task failed: {e}"))?;
    Ok(map)
}

/// Returns up to `limit` (default 50, hard-capped at 50) symbol hits matching
/// `query` case-insensitively. Used by the @-picker's "Symbols" chip.
#[tauri::command]
pub async fn repo_symbols(
    root: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<SymbolHit>, String> {
    let project_root = validate_root(&root)?;
    let cap = limit.unwrap_or(DEFAULT_SYMBOL_LIMIT);
    let hits = tokio::task::spawn_blocking(move || repo_symbols_impl(&project_root, &query, cap))
        .await
        .map_err(|e| format!("repo_symbols task failed: {e}"))?;
    Ok(hits)
}

/// Returns a compact text rendering suitable for direct injection into a chat
/// system prompt. Capped at ~50KB.
#[tauri::command]
pub async fn repo_map_text(project_root: String) -> Result<String, String> {
    let root = validate_root(&project_root)?;
    let text = tokio::task::spawn_blocking(move || {
        let map = compute_repo_map(&root, DEFAULT_MAX_FILES);
        format_as_text(&map)
    })
    .await
    .map_err(|e| format!("repo_map_text task failed: {e}"))?;
    Ok(text)
}
