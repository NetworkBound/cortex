//! Git pull command for the Source Control panel's Pull button.
//!
//! A thin shell-out mirroring [`crate::commands::git_push`]:
//!
//! - [`git_pull`] runs `git pull --ff-only` and returns a structured
//!   [`PullResult`] so the frontend can render the tail of stdout / stderr
//!   inline. `--ff-only` is used deliberately to avoid surprise merge commits:
//!   if the local branch can't fast-forward, git exits non-zero and we hand
//!   the stderr back to the UI instead of opening a merge.
//!
//! Keeps the same `Result<_, String>` shape as the rest of the command bus and
//! never spawns `git` against caller-supplied flags.

use std::path::PathBuf;

use serde::Serialize;

/// Tail length for stdout / stderr blobs returned to the frontend. Pull output
/// is usually short, but non-fast-forward rejections can be wordy.
const TAIL_BYTES: usize = 4 * 1024;

/// Structured result of a `git pull`. `ok` is `true` iff git exited 0;
/// stdout / stderr are tail-trimmed to keep the IPC payload small.
#[derive(Debug, Clone, Serialize)]
pub struct PullResult {
    pub ok: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_code: i32,
}

/// `git pull --ff-only`. Prefers a fast-forward so we never silently create a
/// merge commit; a non-fast-forward situation surfaces as `ok: false` with the
/// rejection in `stderr_tail`.
#[tauri::command]
pub async fn git_pull(project_root: String) -> Result<PullResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let output = crate::sys::no_window("git")
        .args(["pull", "--ff-only"])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git pull: spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(PullResult {
        ok: output.status.success(),
        stdout_tail: tail(stdout, TAIL_BYTES),
        stderr_tail: tail(stderr, TAIL_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Truncate from the front, keeping the last `limit` bytes. Pull output is most
/// interesting at the bottom (rejections, hints, …) so we trim the head rather
/// than the tail. Respects UTF-8 boundaries.
fn tail(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = s.len() - limit;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.replace_range(..cut, "[…truncated…]\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_short_string_intact() {
        let s = "abc".to_string();
        assert_eq!(tail(s.clone(), 100), s);
    }

    #[test]
    fn tail_keeps_last_chunk_with_marker() {
        let s = "x".repeat(TAIL_BYTES + 200);
        let out = tail(s, TAIL_BYTES);
        assert!(out.starts_with("[…truncated…]"));
        assert!(out.ends_with('x'));
    }
}
