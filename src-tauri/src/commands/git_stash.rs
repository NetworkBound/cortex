//! Git stash manager commands powering the `/stash` slash modal.
//!
//! Five thin shell-outs over `git stash`, all keeping the same
//! `Result<_, String>` shape as the rest of the command bus:
//!
//! - [`git_stash_list`] enumerates stashes via
//!   `git stash list --format='%gd|%s|%cr'` and best-effort counts files
//!   touched by parsing `git stash show --stat <ref>`.
//! - [`git_stash_apply`] / [`git_stash_pop`] / [`git_stash_drop`] are the
//!   per-entry actions invoked from the modal's per-row buttons.
//! - [`git_stash_save`] is the "Stash current changes" header action with an
//!   optional message and an `--include-untracked` toggle.
//! - [`git_stash_show`] returns the truncated diff text for the Diff button.
//!
//! Every `ref_id` is sanity-checked against a leading `-` so a stray flag
//! can't be smuggled into `git stash <verb> <ref>`. The project root is
//! validated to be a directory before any subprocess is spawned.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Max length for stdout/stderr blobs returned to the frontend. Stash
/// output is normally small; this just protects against a runaway `show`.
const TAIL_BYTES: usize = 8 * 1024;

/// Max diff size returned by `git_stash_show`. 32 KiB matches the spec's
/// "code block in the modal" cap so the React renderer doesn't blow up on
/// a giant stash.
const SHOW_MAX_BYTES: usize = 32 * 1024;

/// One row in the stash list.
#[derive(Debug, Clone, Serialize)]
pub struct Stash {
    /// Git stash ref, e.g. `stash@{0}`.
    pub ref_id: String,
    /// Subject line — typically `WIP on <branch>: <sha> <msg>` or the
    /// `-m` message supplied at save time.
    pub subject: String,
    /// Relative-time string from git (`%cr`), e.g. `3 hours ago`.
    pub age: String,
    /// Best-effort count of files touched by the stash. `0` when parsing
    /// `git stash show --stat` failed; not an error.
    pub files_changed: u32,
}

/// Result of a mutating stash op (apply / pop / drop / save).
#[derive(Debug, Clone, Serialize)]
pub struct StashOpResult {
    pub ok: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_code: i32,
}

/// `git stash list` parsed into structured rows.
#[tauri::command]
pub async fn git_stash_list(project_root: String) -> Result<Vec<Stash>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let output = crate::sys::no_window("git")
        .args(["stash", "list", "--format=%gd|%s|%cr"])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git stash list: spawn failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git stash list failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut stashes = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let ref_id = match parts.next() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let subject = parts.next().unwrap_or("").to_string();
        let age = parts.next().unwrap_or("").to_string();
        let files_changed = count_files_in_stash(&root, &ref_id);
        stashes.push(Stash {
            ref_id,
            subject,
            age,
            files_changed,
        });
    }
    Ok(stashes)
}

/// `git stash apply <ref>`.
#[tauri::command]
pub async fn git_stash_apply(
    project_root: String,
    ref_id: String,
) -> Result<StashOpResult, String> {
    run_stash_verb(&project_root, "apply", Some(&ref_id), &[])
}

/// `git stash pop <ref>`.
#[tauri::command]
pub async fn git_stash_pop(
    project_root: String,
    ref_id: String,
) -> Result<StashOpResult, String> {
    run_stash_verb(&project_root, "pop", Some(&ref_id), &[])
}

/// `git stash drop <ref>`.
#[tauri::command]
pub async fn git_stash_drop(
    project_root: String,
    ref_id: String,
) -> Result<StashOpResult, String> {
    run_stash_verb(&project_root, "drop", Some(&ref_id), &[])
}

/// `git stash push [-m <msg>] [--include-untracked]`.
#[tauri::command]
pub async fn git_stash_save(
    project_root: String,
    message: Option<String>,
    include_untracked: Option<bool>,
) -> Result<StashOpResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let mut args: Vec<String> = vec!["stash".into(), "push".into()];
    if include_untracked.unwrap_or(false) {
        args.push("--include-untracked".into());
    }
    let msg_trimmed = message.as_deref().map(str::trim).unwrap_or("");
    if !msg_trimmed.is_empty() {
        args.push("-m".into());
        args.push(msg_trimmed.to_string());
    }

    let output = crate::sys::no_window("git")
        .args(&args)
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git stash push: spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(StashOpResult {
        ok: output.status.success(),
        stdout_tail: tail(stdout, TAIL_BYTES),
        stderr_tail: tail(stderr, TAIL_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// `git stash show -p <ref>` — truncated diff text for the modal's Diff
/// button. Returns the (possibly truncated) UTF-8 lossy text. Truncation
/// is signalled by a trailing comment, so the frontend can render the
/// result inside a code block without further processing.
#[tauri::command]
pub async fn git_stash_show(
    project_root: String,
    ref_id: String,
) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    if ref_id.starts_with('-') {
        return Err(format!("refusing ref arg that looks like a flag: {ref_id}"));
    }

    let output = crate::sys::no_window("git")
        .args(["stash", "show", "-p", &ref_id])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git stash show: spawn failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git stash show failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(truncate_diff(stdout, SHOW_MAX_BYTES))
}

// ── helpers ────────────────────────────────────────────────────────────

/// Shared spawn for the per-entry `apply`/`pop`/`drop` verbs. Validates
/// `project_root` is a directory and refuses any `ref_id` that looks like
/// a flag to defang `git stash <verb> -<flag>` injection.
fn run_stash_verb(
    project_root: &str,
    verb: &str,
    ref_id: Option<&str>,
    extra: &[&str],
) -> Result<StashOpResult, String> {
    let root = PathBuf::from(project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let mut args: Vec<&str> = vec!["stash", verb];
    args.extend_from_slice(extra);
    if let Some(r) = ref_id {
        if r.starts_with('-') {
            return Err(format!("refusing ref arg that looks like a flag: {r}"));
        }
        if !r.is_empty() {
            args.push(r);
        }
    }

    let output = crate::sys::no_window("git")
        .args(&args)
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git stash {verb}: spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(StashOpResult {
        ok: output.status.success(),
        stdout_tail: tail(stdout, TAIL_BYTES),
        stderr_tail: tail(stderr, TAIL_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Best-effort count of files in a stash by parsing `git stash show --stat
/// <ref>`. The trailing summary line looks like `N files changed, …`.
/// Returns `0` on any parse or spawn failure — the caller treats this as
/// advisory metadata.
fn count_files_in_stash(root: &Path, ref_id: &str) -> u32 {
    if ref_id.starts_with('-') {
        return 0;
    }
    let output = match crate::sys::no_window("git")
        .args(["stash", "show", "--stat", ref_id])
        .current_dir(root)
        .output()
    {
        Ok(o) => o,
        Err(_) => return 0,
    };
    if !output.status.success() {
        return 0;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The last non-empty line is `N files changed, …` (or `N file changed,
    // …` for the singular form). Fall back to counting per-file `+/-`
    // rows if the summary footer can't be parsed.
    let trimmed = stdout.trim_end();
    if let Some(last_line) = trimmed.lines().rev().find(|l| !l.trim().is_empty()) {
        let trimmed_line = last_line.trim_start();
        let first_word = trimmed_line.split_whitespace().next().unwrap_or("");
        if let Ok(n) = first_word.parse::<u32>() {
            if trimmed_line.contains("file changed") || trimmed_line.contains("files changed") {
                return n;
            }
        }
    }
    // Fallback: every per-file stat row contains `|` followed by a count
    // and at least one `+` or `-`. Counting those rows is close enough.
    let mut count: u32 = 0;
    for line in trimmed.lines() {
        if !line.contains('|') {
            continue;
        }
        let after_bar = line.split('|').nth(1).unwrap_or("");
        if after_bar.contains('+') || after_bar.contains('-') {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Truncate from the front, keeping the last `limit` bytes at a UTF-8
/// boundary. `git push` style: trims the head when the blob is over budget.
fn tail(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = s.len() - limit;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.drain(..cut);
    s
}

/// Truncate a diff blob to `limit` bytes, keeping the *head* (the start of
/// the diff is the most informative chunk). A trailing comment line is
/// appended so the frontend can show the truncation in-place.
fn truncate_diff(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n# … diff truncated at 32 KiB …\n");
    s
}
