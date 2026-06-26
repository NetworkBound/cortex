//! Tauri command wrappers around [`crate::git`].
//!
//! Each handler is a thin async shim that converts the request payload into a
//! `Path` + calls into the sync implementation. Returning `Result<_, String>`
//! matches the existing command-bus convention.

use std::path::PathBuf;

use crate::git::{self, Commit, CommitFile, WorkingStatus};

#[tauri::command]
pub async fn git_history(
    project_root: String,
    limit: u32,
    offset: Option<u32>,
) -> Result<Vec<Commit>, String> {
    let root = PathBuf::from(&project_root);
    git::history(&root, limit, offset.unwrap_or(0))
}

#[tauri::command]
pub async fn git_show(project_root: String, hash: String) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    git::show_commit(&root, &hash)
}

#[tauri::command]
pub async fn git_commit_files(
    project_root: String,
    hash: String,
) -> Result<Vec<CommitFile>, String> {
    let root = PathBuf::from(&project_root);
    git::commit_files(&root, &hash)
}

#[tauri::command]
pub async fn git_commit_file_diff(
    project_root: String,
    hash: String,
    path: String,
) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    git::commit_file_diff(&root, &hash, &path)
}

#[tauri::command]
pub async fn git_working_status(project_root: String) -> Result<WorkingStatus, String> {
    let root = PathBuf::from(&project_root);
    git::working_status(&root)
}

#[tauri::command]
pub async fn git_stage_file(project_root: String, path: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    git::stage_file(&root, &path)
}

#[tauri::command]
pub async fn git_unstage_file(project_root: String, path: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    git::unstage_file(&root, &path)
}

#[tauri::command]
pub async fn git_discard_changes(project_root: String, path: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    git::discard_changes(&root, &path)
}

#[tauri::command]
pub async fn git_commit(project_root: String, message: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    git::commit_staged(&root, &message)
}

/// Unified diff for one file. `mode` is `"staged"`, `"unstaged"`, or
/// `"untracked"` (the latter synthesizes an all-additions patch).
#[tauri::command]
pub async fn git_file_diff(
    project_root: String,
    path: String,
    mode: String,
) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    let mode = git::DiffMode::parse(&mode)?;
    git::file_diff(&root, &path, mode)
}
