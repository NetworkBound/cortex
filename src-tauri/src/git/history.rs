//! `git log` walker — returns a flat `Vec<Commit>` for the history panel.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Hard cap on rows returned. Keeps the UI list snappy even on huge repos.
const MAX_COMMITS: u32 = 200;
/// Hard cap on the diff blob returned by [`show_commit`]. 16 KiB matches the
/// brief — bigger diffs get truncated with a `[truncated]` marker.
const SHOW_LIMIT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub age: String,
    pub subject: String,
    pub refs: Vec<String>,
    pub parents: Vec<String>,
}

/// One changed file within a commit, as reported by `git show --name-status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFile {
    /// Path of the file (the new path for renames).
    pub path: String,
    /// Single-letter git status: `A`/`M`/`D`/`R`/`C`/`T`.
    pub status: String,
}

/// Run `git log` and parse the pipe-delimited output. Returns an empty Vec on
/// any failure (not a repo, git missing, bad project path) — the frontend
/// treats both "empty" and "error" the same way.
///
/// `offset` skips that many commits from the tip before collecting `limit`
/// rows, so the UI can paginate ("load more") deeper into history. The
/// returned page is always capped at [`MAX_COMMITS`] rows.
pub fn history(project_root: &Path, limit: u32, offset: u32) -> Result<Vec<Commit>, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    // A limit of 0 means "use the default cap" rather than "return one row".
    // Any positive value is honoured up to the hard cap.
    let limit = if limit == 0 {
        MAX_COMMITS
    } else {
        limit.min(MAX_COMMITS)
    };

    // `%d` carries the `(HEAD -> main, origin/main, tag: v1)` decoration.
    // `%P` is the space-separated list of parent hashes (multiple for merges).
    // Using `--decorate` + `--no-color` keeps it parse-friendly.
    // We walk only the current branch (HEAD) — `--all` would pull in commits
    // from every ref (other branches, remotes, tags), which the history panel
    // doesn't want. `--skip` advances the page window for pagination.
    let fmt = "%H|%P|%h|%an|%ar|%s|%d";
    let output = crate::sys::no_window("git")
        .args([
            "log",
            &format!("--pretty=format:{fmt}"),
            "--decorate",
            &format!("-{limit}"),
            &format!("--skip={offset}"),
            "--no-color",
        ])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git log: spawn failed: {e}"))?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Split with a max of 7 — `%s` (subject) can contain `|` we want to
        // keep as part of the subject. We split the *last* field (the
        // decoration) off the back instead.
        let parts: Vec<&str> = line.splitn(7, '|').collect();
        if parts.len() < 6 {
            continue;
        }
        let hash = parts[0].to_string();
        let parents: Vec<String> = parts[1]
            .split_whitespace()
            .map(|p| p.to_string())
            .collect();
        let short = parts[2].to_string();
        let author = parts[3].to_string();
        let age = parts[4].to_string();
        // Re-merge any pipe-containing subject + decoration tail. The
        // decoration always starts with ` (` (a space + paren) so we can split
        // there if present.
        let tail = parts[5..].join("|");
        let (subject, refs) = split_decoration(&tail);

        commits.push(Commit {
            hash,
            short_hash: short,
            author,
            age,
            subject: subject.trim().to_string(),
            refs,
            parents,
        });
    }
    Ok(commits)
}

/// Splits `<subject> (HEAD -> main, origin/main)` into subject + refs.
/// The decoration is always at the end and wrapped in `(...)`.
fn split_decoration(s: &str) -> (String, Vec<String>) {
    if let Some(open) = s.rfind(" (") {
        if s.ends_with(')') {
            let subject = s[..open].to_string();
            let inner = &s[open + 2..s.len() - 1];
            let refs = inner
                .split(',')
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .collect();
            return (subject, refs);
        }
    }
    (s.to_string(), Vec::new())
}

/// Run `git show <hash> --stat` and return the (possibly truncated) text.
pub fn show_commit(project_root: &Path, hash: &str) -> Result<String, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    if !is_safe_hash(hash) {
        return Err("invalid commit hash".to_string());
    }
    let output = crate::sys::no_window("git")
        .args(["show", "--stat", "--no-color", hash])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git show: spawn failed: {e}"))?;
    if !output.status.success() {
        return Ok(String::new());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.len() > SHOW_LIMIT_BYTES {
        let mut cut = SHOW_LIMIT_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[truncated — diff exceeded 16 KiB]");
    }
    Ok(text)
}

/// List the files changed in a single commit via `git show --name-status`.
///
/// The merge commits are diffed against the first parent (`-m --first-parent`
/// would explode rows); we accept git's default of showing nothing for merges
/// rather than a combined diff, which keeps the per-file navigation honest.
pub fn commit_files(project_root: &Path, hash: &str) -> Result<Vec<CommitFile>, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    if !is_safe_hash(hash) {
        return Err("invalid commit hash".to_string());
    }
    let output = crate::sys::no_window("git")
        .args([
            "show",
            "--name-status",
            "--no-color",
            "--format=", // suppress the commit header; we only want the file table
            hash,
        ])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git show: spawn failed: {e}"))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Lines look like `M\tsrc/foo.rs` or `R100\told\tnew`. Tab-separated.
        let mut cols = line.split('\t');
        let status_raw = cols.next().unwrap_or("").trim();
        if status_raw.is_empty() {
            continue;
        }
        // First letter is the status code; renames/copies carry a similarity
        // score (e.g. `R100`) we drop. For renames the path we care about is
        // the *new* path, which is the last tab-separated field.
        let status = status_raw.chars().next().unwrap_or('?').to_string();
        let path = match cols.clone().last() {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => continue,
        };
        files.push(CommitFile { status, path });
    }
    Ok(files)
}

/// Return the unified diff for a single file within a commit:
/// `git show <hash> -- <path>`. Truncated to [`SHOW_LIMIT_BYTES`] like the
/// full-commit view.
pub fn commit_file_diff(
    project_root: &Path,
    hash: &str,
    path: &str,
) -> Result<String, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    if !is_safe_hash(hash) {
        return Err("invalid commit hash".to_string());
    }
    if !is_safe_path(path) {
        return Err("invalid file path".to_string());
    }
    let output = crate::sys::no_window("git")
        .args([
            "show",
            "--no-color",
            "--format=", // drop the commit header — we want just the file diff
            hash,
            "--",
            path,
        ])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git show: spawn failed: {e}"))?;
    if !output.status.success() {
        return Ok(String::new());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.len() > SHOW_LIMIT_BYTES {
        let mut cut = SHOW_LIMIT_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[truncated — diff exceeded 16 KiB]");
    }
    Ok(text)
}

/// Reject pathspecs that could be read as a flag or escape the `--` boundary.
/// We already pass `--` before the path so git treats it as a pathspec, but a
/// leading dash or NUL/newline is still worth refusing outright.
fn is_safe_path(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.contains('\0')
        && !s.contains('\n')
        && !s.contains('\r')
}

/// Guard against arg injection and revision-range/ref-expression abuse.
///
/// A real git object hash is 7..=64 hex chars. We accept *only* a bare hex
/// hash, or the single fixed symbolic ref `HEAD`. Notably we reject `.`, `/`,
/// `-`, and anything containing `..`/`...` so `git show <hash>` can never be
/// handed a range (`a..b`), a ref expression (`HEAD~1`, `HEAD@{1}`), a pathspec
/// (`a/b`), or a leading-dash flag.
fn is_safe_hash(s: &str) -> bool {
    // Fixed symbolic ref that callers may legitimately paste.
    if s == "HEAD" {
        return true;
    }
    let len = s.len();
    if len < 7 || len > 64 {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoration_split_basic() {
        let (s, r) = split_decoration("feat: add thing (HEAD -> main, origin/main)");
        assert_eq!(s, "feat: add thing");
        assert_eq!(r, vec!["HEAD -> main", "origin/main"]);
    }

    #[test]
    fn decoration_split_no_refs() {
        let (s, r) = split_decoration("fix: typo");
        assert_eq!(s, "fix: typo");
        assert!(r.is_empty());
    }

    #[test]
    fn safe_hash_rejects_flags() {
        assert!(!is_safe_hash("--rm"));
        assert!(!is_safe_hash(""));
        assert!(is_safe_hash("abc1234"));
        assert!(is_safe_hash("HEAD"));
        // Reject range / ref expressions and pathspecs.
        assert!(!is_safe_hash("HEAD~1"));
        assert!(!is_safe_hash("HEAD@{1}"));
        assert!(!is_safe_hash("abc1234..def5678"));
        assert!(!is_safe_hash("abc1234...def5678"));
        assert!(!is_safe_hash(".."));
        assert!(!is_safe_hash("a/b"));
        assert!(!is_safe_hash("origin/main"));
        assert!(!is_safe_hash("v1.0"));
        // Too short to be a real abbreviated hash.
        assert!(!is_safe_hash("abc"));
        // Non-hex letters are not valid hash chars.
        assert!(!is_safe_hash("zzzzzzz"));
    }

    #[test]
    fn safe_path_rejects_flags_and_control_chars() {
        assert!(is_safe_path("src/foo.rs"));
        assert!(is_safe_path("a b/with spaces.txt"));
        assert!(!is_safe_path(""));
        assert!(!is_safe_path("--output=evil"));
        assert!(!is_safe_path("-rf"));
        assert!(!is_safe_path("foo\0bar"));
        assert!(!is_safe_path("foo\nbar"));
    }
}
