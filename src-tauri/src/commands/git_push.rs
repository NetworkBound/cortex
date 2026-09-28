//! Git commit + push commands for the `/commit`, `/push`, and `/ship` slashes.
//!
//! Two thin shell-outs:
//!
//! - [`git_commit_staged`] runs `git commit -m <msg>` (no `--no-verify`) so
//!   pre-commit hooks still fire. Delegates to [`crate::git::commit_staged`]
//!   so we share the empty-message guard + path validation with the rest of
//!   the working-tree command surface.
//! - [`git_push`] runs `git push origin <branch || HEAD>` and returns a
//!   structured [`PushResult`] so the frontend can render the tail of stdout
//!   / stderr inline. Force-push is opt-in via the `force` flag (the slash
//!   command only enables it when the user types `/push --force`).
//!
//! Both commands keep the same `Result<_, String>` shape as the rest of the
//! command bus and never spawn `git` against caller-supplied flags — the
//! `branch` arg is sanity-checked against a leading `-` to keep `git push`
//! from interpreting it as an option.

use std::path::PathBuf;

use serde::Serialize;

use crate::commands::git::{tail_output as tail, NON_INTERACTIVE_ENV};

/// Tail length for stdout / stderr blobs returned to the frontend. Push
/// output is usually short, but force-with-lease rejections can be wordy.
const TAIL_BYTES: usize = 4 * 1024;

/// Structured result of a `git push`. `ok` is `true` iff git exited 0;
/// stdout / stderr are tail-trimmed to keep the IPC payload small.
#[derive(Debug, Clone, Serialize)]
pub struct PushResult {
    pub ok: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_code: i32,
    /// Branch we actually pushed (resolves `None` to `HEAD`).
    pub branch: String,
}

/// `git commit -m <msg>` against the staged index. Reuses the shared helper
/// so the empty-message + not-a-directory guards stay in one place.
#[tauri::command]
pub async fn git_commit_staged(project_root: String, message: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    crate::git::commit_staged(&root, &message)
}

/// `git push origin <branch || HEAD>`. Set `force=true` to add `--force`.
/// Refuses any `branch` value starting with `-` to defang flag-injection.
#[tauri::command]
pub async fn git_push(
    project_root: String,
    branch: Option<String>,
    force: Option<bool>,
) -> Result<PushResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let target = branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD")
        .to_string();
    if target.starts_with('-') {
        return Err(format!(
            "refusing branch arg that looks like a flag: {target}"
        ));
    }

    let mut args: Vec<&str> = vec!["push", "origin", &target];
    if force.unwrap_or(false) {
        args.push("--force");
    }

    let output = crate::sys::no_window("git")
        .args(&args)
        .envs(NON_INTERACTIVE_ENV.iter().copied())
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git push: spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(PushResult {
        ok: output.status.success(),
        stdout_tail: tail(stdout, TAIL_BYTES),
        stderr_tail: tail(stderr, TAIL_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
        branch: target,
    })
}
