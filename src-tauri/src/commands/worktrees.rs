use crate::observability::tracing_store::TracingStore;
use crate::worktrees::{self, Worktree, WorktreeStore};
use serde::Deserialize;
use std::path::PathBuf;
use tauri::State;

fn store_from(s: &TracingStore) -> WorktreeStore {
    WorktreeStore::new(s.shared_connection())
}

/// Validate and confine a frontend-supplied `project_root` before running git
/// or creating directories at that location.
///
/// The path is canonicalized (which also resolves any `..` traversal) and must:
/// - resolve to an existing directory,
/// - live under the user's home directory (the only place projects are expected),
/// - contain a `.git` entry, so we only operate on real git project roots.
fn confine_project_root(project_root: &str) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "could not determine home directory".to_string())?;
    let home = home
        .canonicalize()
        .map_err(|e| format!("could not resolve home directory: {e}"))?;

    let root = PathBuf::from(project_root)
        .canonicalize()
        .map_err(|e| format!("invalid project_root '{project_root}': {e}"))?;

    if !root.is_dir() {
        return Err(format!("project_root '{project_root}' is not a directory"));
    }

    if !root.starts_with(&home) {
        return Err(format!(
            "project_root '{project_root}' is outside the allowed base directory"
        ));
    }

    if !root.join(".git").exists() {
        return Err(format!(
            "project_root '{project_root}' is not a git repository"
        ));
    }

    Ok(root)
}

#[tauri::command]
pub async fn list_worktrees(
    project_root: Option<String>,
    store: State<'_, TracingStore>,
) -> Result<Vec<Worktree>, String> {
    let ws = store_from(&store);
    ws.list_active(project_root.as_deref()).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct CreateWorktreeArgs {
    pub project_root: String,
    pub note: Option<String>,
}

#[tauri::command]
pub async fn create_worktree(
    args: CreateWorktreeArgs,
    store: State<'_, TracingStore>,
) -> Result<Worktree, String> {
    let ws = store_from(&store);
    let root = confine_project_root(&args.project_root)?;
    worktrees::create_worktree(&ws, &root, args.note).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct RemoveWorktreeArgs {
    pub id: String,
    #[serde(default)]
    pub archive_commit: bool,
}

#[tauri::command]
pub async fn remove_worktree(
    args: RemoveWorktreeArgs,
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    let ws = store_from(&store);
    worktrees::remove_worktree(&ws, &args.id, args.archive_commit).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct AssignWorktreeArgs {
    pub id: String,
    pub session_id: String,
}

#[tauri::command]
pub async fn assign_worktree_session(
    args: AssignWorktreeArgs,
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    let ws = store_from(&store);
    ws.assign_session(&args.id, &args.session_id).map_err(|e| e.to_string())
}
