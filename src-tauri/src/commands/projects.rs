use crate::app_state::AppState;
use crate::projects::{
    discover_projects, ignore_status, list_files, rules, vault_root, CortexIgnoreStatus,
    FileTreeEntry, ProjectMeta,
};
use crate::projects::rules::RuleSummary;
use std::path::PathBuf;
use tauri::State;

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectMeta>, String> {
    // Source the vault from the auto-detected app config (Obsidian's own
    // registry → OBSIDIAN_VAULT → ~/vault). discover_projects falls back to
    // the env-var heuristic if this is None, so the DEFAULT build is unchanged.
    let vault_root = state.config.read().obsidian_vault.clone();
    Ok(discover_projects(vault_root))
}

/// Read a vault project note's markdown for injection as chat context.
/// The path is canonicalized and CONFINED to the vault root so a crafted
/// path can't escape the vault and exfiltrate arbitrary files. Caps the
/// returned body at ~200 KB.
#[tauri::command]
pub async fn open_vault_note(path: String) -> Result<String, String> {
    const MAX_BYTES: usize = 200 * 1024;
    let vault = vault_root().ok_or_else(|| "no vault root configured".to_string())?;
    let vault_canon =
        std::fs::canonicalize(&vault).map_err(|e| format!("vault root unreadable: {e}"))?;
    let target = std::fs::canonicalize(PathBuf::from(&path))
        .map_err(|e| format!("cannot resolve note path: {e}"))?;
    if !target.starts_with(&vault_canon) {
        return Err(format!("path escapes vault: {path}"));
    }
    let mut body = std::fs::read_to_string(&target).map_err(|e| format!("read failed: {e}"))?;
    if body.len() > MAX_BYTES {
        body.truncate(MAX_BYTES);
        body.push_str("\n\n… (truncated)");
    }
    Ok(body)
}

#[tauri::command]
pub async fn set_active_project(path: String, state: State<'_, AppState>) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() || !p.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    state.config.write().default_project_root = Some(p.clone());
    // Persist to ~/.cortex/last-project.json so the choice survives restart.
    // Disk failure here is non-fatal — the in-memory selection still works
    // for this session; just log and continue.
    if let Err(e) = AppState::save_default_project_root(&p) {
        tracing::warn!("failed to persist active project: {e}");
    }
    Ok(())
}

#[tauri::command]
pub async fn project_files(path: String, limit: Option<usize>) -> Result<Vec<FileTreeEntry>, String> {
    let p = PathBuf::from(&path);
    if !p.exists() { return Err(format!("missing: {path}")); }
    Ok(list_files(&p, limit.unwrap_or(500)))
}

/// Lists every `.cortex/rules/*.md` rule with its activation metadata so the
/// Workspace settings tab can render badges (always / globs / desc / manual).
/// Rules without YAML frontmatter default to `alwaysApply` for backward
/// compatibility with the original Cortex rule loader.
#[tauri::command]
pub async fn list_rules(project_root: String) -> Result<Vec<RuleSummary>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(rules::load_rules(&root).iter().map(|r| r.summary()).collect())
}

/// Returns the merged `.cortexignore` status for `project_root` — surfaced
/// in the UI as a chip next to the project name. Loads both `~/.cortex/cortexignore`
/// (global) and `<project>/.cortexignore`.
#[tauri::command]
pub async fn cortexignore_status(project_root: String) -> Result<CortexIgnoreStatus, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(ignore_status(&root))
}
