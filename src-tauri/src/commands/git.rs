//! Tauri command wrappers around [`crate::git`].
//!
//! Each handler is a thin async shim that converts the request payload into a
//! `Path` + calls into the sync implementation. Returning `Result<_, String>`
//! matches the existing command-bus convention.

use std::path::PathBuf;

use crate::git::{self, Commit, CommitFile, WorkingStatus};

/// Keep the last `limit` bytes of a subprocess blob (stdout / stderr), cut at
/// a UTF-8 boundary and prefixed with a truncation marker. Shared by the
/// `git_pull` / `git_push` / `clone_git_repo` / `git stash` commands: git's
/// most useful output (rejections, hints, …) sits at the *bottom*, so we trim
/// the head rather than the tail.
pub(crate) fn tail_output(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = s.len() - limit;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.replace_range(..cut, "[…truncated…]\n");
    s
}

/// Environment that keeps a non-interactive `git` from hanging on a
/// credential prompt: with no terminal attached, `git` would otherwise block
/// forever waiting on stdin (`GIT_TERMINAL_PROMPT=0` makes it fail fast), and
/// on Windows the Git Credential Manager would pop a modal dialog behind the
/// app (`GCM_INTERACTIVE=never`). Apply to any network-facing git shell-out.
pub(crate) const NON_INTERACTIVE_ENV: &[(&str, &str)] =
    &[("GIT_TERMINAL_PROMPT", "0"), ("GCM_INTERACTIVE", "never")];

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

#[cfg(test)]
mod tests {
    use super::tail_output;

    #[test]
    fn tail_returns_short_string_intact() {
        let s = "abc".to_string();
        assert_eq!(tail_output(s.clone(), 100), s);
    }

    #[test]
    fn tail_keeps_last_chunk_with_marker() {
        let s = "x".repeat(4096 + 200);
        let out = tail_output(s, 4096);
        assert!(out.starts_with("[…truncated…]"));
        assert!(out.ends_with('x'));
    }

    #[test]
    fn tail_respects_utf8_boundary() {
        let mut s = String::new();
        // Just over the limit, ending in multi-byte chars so the cut point
        // lands mid-codepoint unless the boundary walk moves it.
        for _ in 0..2048 {
            s.push('a');
        }
        for _ in 0..(2048 + 50) {
            s.push('é'); // 2 bytes each
        }
        let out = tail_output(s, 4096);
        assert!(out.starts_with("[…truncated…]"));
        assert!(out.ends_with('é'));
    }
}
