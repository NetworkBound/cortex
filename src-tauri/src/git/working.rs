//! `git status` parsing + stage/unstage/discard/commit helpers.
//!
//! All shell-outs return `Result<_, String>` with a human-readable message —
//! the frontend surfaces these in a toast on failure.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    /// Single-letter status code from porcelain v1 (`M`, `A`, `D`, `R`, `?`).
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    pub untracked: Vec<String>,
}

/// Parse `git status --porcelain=v1 -b` into a structured snapshot.
pub fn working_status(project_root: &Path) -> Result<WorkingStatus, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    let output = crate::sys::no_window("git")
        .args(["status", "--porcelain=v1", "-b"])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git status: spawn failed: {e}"))?;
    if !output.status.success() {
        return Ok(WorkingStatus {
            branch: "(not a repo)".into(),
            ahead: 0,
            behind: 0,
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_porcelain(&stdout))
}

fn parse_porcelain(text: &str) -> WorkingStatus {
    let mut branch = String::from("(detached)");
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut staged = Vec::<FileEntry>::new();
    let mut unstaged = Vec::<FileEntry>::new();
    let mut untracked = Vec::<String>::new();

    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("## ") {
            // Branch header: "main...origin/main [ahead 2, behind 1]" or
            // just "HEAD (no branch)" when detached.
            let (b, a, be) = parse_branch_header(rest);
            branch = b;
            ahead = a;
            behind = be;
            continue;
        }
        if raw.len() < 3 {
            continue;
        }
        // porcelain v1 lines are `XY <path>` where X = staged status, Y = unstaged.
        // `raw` may not be ASCII (paths can contain UTF-8), so index by bytes for
        // the two status columns but slice the path on a char boundary.
        let bytes = raw.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        // Column 2 is a space separator; the path begins at byte 3. Guard the
        // boundary in case of a malformed short line.
        if !raw.is_char_boundary(3) {
            continue;
        }
        let path = normalize_porcelain_path(&raw[3..]);

        if x == '?' && y == '?' {
            untracked.push(path);
            continue;
        }
        if x != ' ' && x != '?' {
            staged.push(FileEntry {
                path: path.clone(),
                status: x.to_string(),
            });
        }
        if y != ' ' && y != '?' {
            unstaged.push(FileEntry {
                path,
                status: y.to_string(),
            });
        }
    }

    WorkingStatus {
        branch,
        ahead,
        behind,
        staged,
        unstaged,
        untracked,
    }
}

/// Extract the working-tree path from a porcelain v1 entry's path field.
///
/// Renamed/copied entries (status `R`/`C`) render as `orig -> new`; we want the
/// destination (`new`). Paths containing special or non-ASCII bytes are rendered
/// double-quoted with C-style escapes when `core.quotePath` is enabled; we unquote
/// those so callers get the real path. A plain path is returned unchanged.
fn normalize_porcelain_path(field: &str) -> String {
    // For renames/copies, keep the destination side of the arrow.
    let target = match field.rsplit_once(" -> ") {
        Some((_orig, new)) => new,
        None => field,
    };
    unquote_c_path(target)
}

/// Decode a git C-quoted path (e.g. `"sp\303\244ce.txt"`). Returns the input
/// unchanged if it is not a quoted string.
fn unquote_c_path(s: &str) -> String {
    let trimmed = s.trim();
    if !(trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2) {
        return s.to_string();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('r') => bytes.push(b'\r'),
            Some('"') => bytes.push(b'"'),
            Some('\\') => bytes.push(b'\\'),
            // Octal escape: up to three digits, e.g. `\303`.
            Some(d) if d.is_digit(8) => {
                let mut val = d.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.clone().next() {
                        Some(n) if n.is_digit(8) => {
                            val = val * 8 + n.to_digit(8).unwrap();
                            chars.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(val as u8);
            }
            // Unknown escape: keep the backslash and the following char verbatim.
            Some(other) => {
                bytes.push(b'\\');
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    // git emits UTF-8 octal bytes; recover the string, falling back lossily.
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn parse_branch_header(s: &str) -> (String, u32, u32) {
    if s.starts_with("HEAD") {
        return ("(detached)".into(), 0, 0);
    }
    // Split off the "[ahead N, behind M]" tail.
    let (head, tail) = match s.find(" [") {
        Some(i) => {
            // Strip the trailing `]` only if it's actually present, so headers
            // without a closing bracket don't lose their last character.
            let inner = s[i + 2..].strip_suffix(']').unwrap_or(&s[i + 2..]);
            (&s[..i], Some(inner))
        }
        None => (s, None),
    };
    // Branch is whatever's before `...` (the upstream marker).
    let branch = head.split("...").next().unwrap_or(head).to_string();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    if let Some(t) = tail {
        for part in t.split(", ") {
            if let Some(rest) = part.strip_prefix("ahead ") {
                ahead = rest.trim().parse().unwrap_or(0);
            } else if let Some(rest) = part.strip_prefix("behind ") {
                behind = rest.trim().parse().unwrap_or(0);
            }
        }
    }
    (branch, ahead, behind)
}

/// `git add -- <path>`
pub fn stage_file(project_root: &Path, path: &str) -> Result<(), String> {
    run_git(project_root, &["add", "--", path])
}

/// `git reset HEAD -- <path>` (works pre-first-commit too)
pub fn unstage_file(project_root: &Path, path: &str) -> Result<(), String> {
    run_git(project_root, &["reset", "HEAD", "--", path])
}

/// `git checkout -- <path>` — drops unstaged changes for that path. Best-effort.
pub fn discard_changes(project_root: &Path, path: &str) -> Result<(), String> {
    run_git(project_root, &["checkout", "--", path])
}

/// Which side of the index [`file_diff`] should compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// `git diff --cached` — index vs HEAD (what a commit would contain).
    Staged,
    /// `git diff` — working tree vs index.
    Unstaged,
    /// File not yet tracked — synthesize an all-additions diff.
    Untracked,
}

impl DiffMode {
    /// Parse the frontend's string form. Kept here (not in the command shim)
    /// so it's unit-testable alongside the rest of the diff logic.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "staged" => Ok(Self::Staged),
            "unstaged" => Ok(Self::Unstaged),
            "untracked" => Ok(Self::Untracked),
            other => Err(format!("unknown diff mode: {other:?}")),
        }
    }
}

/// Cap on the diff text we ship to the webview. Big enough for any human
/// review; beyond it the pane shows a truncation notice instead of freezing.
const DIFF_CAP_BYTES: usize = 200 * 1024;

/// Unified diff for a single file, staged or unstaged as appropriate.
///
/// Untracked files have no diff in git's eyes, so we synthesize an
/// all-additions hunk from the file contents (binary files get a stub).
pub fn file_diff(project_root: &Path, path: &str, mode: DiffMode) -> Result<String, String> {
    match mode {
        DiffMode::Staged => {
            let out = run_git_capture(
                project_root,
                &["diff", "--no-color", "--cached", "--", path],
            )?;
            Ok(cap_diff_text(out))
        }
        DiffMode::Unstaged => {
            let out = run_git_capture(project_root, &["diff", "--no-color", "--", path])?;
            Ok(cap_diff_text(out))
        }
        DiffMode::Untracked => untracked_diff(project_root, path),
    }
}

/// Build a `git diff`-shaped all-additions patch for an untracked file.
fn untracked_diff(project_root: &Path, path: &str) -> Result<String, String> {
    ensure_within_root(project_root, path)?;
    let full = project_root.join(path);
    let bytes = std::fs::read(&full).map_err(|e| format!("read {path}: {e}"))?;
    // Same heuristic git uses: a NUL byte near the front means binary.
    if bytes.iter().take(8_000).any(|b| *b == 0) {
        return Ok(format!(
            "diff --git a/{path} b/{path}\nnew file (binary, {} bytes)\n",
            bytes.len()
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(cap_diff_text(synthesize_add_diff(path, &text)))
}

/// Render `content` as a unified diff where every line is an addition —
/// what `git diff` would show for this file right after `git add -N`.
fn synthesize_add_diff(path: &str, content: &str) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\nnew file\n--- /dev/null\n+++ b/{path}\n");
    if content.is_empty() {
        return out;
    }
    let ends_with_newline = content.ends_with('\n');
    let body = if ends_with_newline {
        &content[..content.len() - 1]
    } else {
        content
    };
    let lines: Vec<&str> = body.split('\n').collect();
    out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in &lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    if !ends_with_newline {
        out.push_str("\\ No newline at end of file\n");
    }
    out
}

/// Truncate oversized diff text on a line boundary and append a notice.
fn cap_diff_text(diff: String) -> String {
    if diff.len() <= DIFF_CAP_BYTES {
        return diff;
    }
    // Cut at the last full line within the cap so we never split a UTF-8
    // char or leave a dangling half-row in the rendered pane.
    let cut = diff[..DIFF_CAP_BYTES]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or_else(|| {
            // Single enormous line: back off to the nearest char boundary.
            let mut i = DIFF_CAP_BYTES;
            while !diff.is_char_boundary(i) {
                i -= 1;
            }
            i
        });
    let mut out = diff[..cut].to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("… diff truncated (first 200 KiB shown)\n");
    out
}

/// `git commit -m <msg>` — commits the staged index.
pub fn commit_staged(project_root: &Path, message: &str) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err("commit message must not be empty".into());
    }
    run_git(project_root, &["commit", "-m", trimmed])
}

/// Reject a caller-supplied path that would resolve outside `project_root`.
///
/// Paths from git status are repo-relative, but the public stage/discard helpers
/// take an arbitrary `&str`. We refuse absolute paths and any `..` traversal so a
/// tainted value cannot make git operate on files outside the project. The check
/// is lexical (the leaf may legitimately not exist on disk, e.g. a staged delete),
/// but `project_root` itself is canonicalized to collapse symlinks/`.` segments.
fn ensure_within_root(project_root: &Path, path: &str) -> Result<(), String> {
    use std::path::Component;

    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!("refusing path outside project root: {path:?}"));
    }

    // Lexically resolve the relative path, tracking depth below the root. Any
    // point where we would pop above the root (depth < 0) is a traversal.
    let mut depth: i32 = 0;
    for comp in candidate.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(format!("refusing path outside project root: {path:?}"));
                }
            }
            // Prefix/RootDir can't appear in a relative path, but be strict.
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("refusing path outside project root: {path:?}"));
            }
        }
    }

    // Defense in depth: if the resolved location exists, confirm it really is
    // under the canonical root (catches symlink escapes the lexical check misses).
    if let Ok(root) = project_root.canonicalize() {
        let joined = root.join(candidate);
        if let Ok(resolved) = joined.canonicalize() {
            if !resolved.starts_with(&root) {
                return Err(format!("refusing path outside project root: {path:?}"));
            }
        }
    }
    Ok(())
}

/// Shared safety gate for caller-supplied args. Refuses any arg that looks
/// like a flag once we're past the `--` separator: everything before `--` is
/// a fixed string literal supplied by us (subcommand + flags); everything
/// after it is caller/user-supplied (paths). Treating a tainted path
/// beginning with `-` as a flag would let it smuggle options into git, so we
/// reject it outright. Post-`--` paths are also containment-checked so a
/// value like `../../etc/passwd` cannot make git touch files outside the
/// repository we were handed.
fn guard_args(project_root: &Path, args: &[&str]) -> Result<(), String> {
    let mut after_separator = false;
    for a in args.iter() {
        if !after_separator {
            if *a == "--" {
                after_separator = true;
            }
            continue;
        }
        if a.starts_with('-') {
            return Err(format!(
                "refusing git arg that looks like a flag: {a:?}"
            ));
        }
        ensure_within_root(project_root, a)?;
    }
    Ok(())
}

fn run_git(project_root: &Path, args: &[&str]) -> Result<(), String> {
    run_git_capture(project_root, args).map(|_| ())
}

/// Like [`run_git`] but returns trimmed-nothing stdout (lossy UTF-8).
fn run_git_capture(project_root: &Path, args: &[&str]) -> Result<String, String> {
    if !project_root.is_dir() {
        return Err(format!("not a directory: {}", project_root.display()));
    }
    guard_args(project_root, args)?;
    let output = crate::sys::no_window("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git {}: spawn failed: {e}", args.join(" ")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_tree() {
        let txt = "## main...origin/main\n";
        let s = parse_porcelain(txt);
        assert_eq!(s.branch, "main");
        assert_eq!(s.ahead, 0);
        assert_eq!(s.behind, 0);
        assert!(s.staged.is_empty());
        assert!(s.unstaged.is_empty());
        assert!(s.untracked.is_empty());
    }

    #[test]
    fn parses_mixed_status() {
        let txt = "\
## feat/x...origin/feat/x [ahead 2, behind 1]
M  src/lib.rs
 M src/main.rs
A  src/new.rs
?? docs/draft.md
";
        let s = parse_porcelain(txt);
        assert_eq!(s.branch, "feat/x");
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 1);
        assert_eq!(s.staged.len(), 2);
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.untracked, vec!["docs/draft.md".to_string()]);
    }

    #[test]
    fn parses_detached() {
        let s = parse_porcelain("## HEAD (no branch)\n");
        assert_eq!(s.branch, "(detached)");
    }

    #[test]
    fn rename_uses_destination_path() {
        let txt = "## main\nR  old/name.rs -> new/name.rs\n";
        let s = parse_porcelain(txt);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].path, "new/name.rs");
    }

    #[test]
    fn unquotes_octal_utf8_path() {
        // git quotes non-ASCII paths; \303\244 is UTF-8 for 'ä'.
        let txt = "## main\n M \"sp\\303\\244ce.txt\"\n";
        let s = parse_porcelain(txt);
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.unstaged[0].path, "späce.txt");
    }

    #[test]
    fn diff_mode_parses_known_values_only() {
        assert_eq!(DiffMode::parse("staged").unwrap(), DiffMode::Staged);
        assert_eq!(DiffMode::parse("unstaged").unwrap(), DiffMode::Unstaged);
        assert_eq!(DiffMode::parse("untracked").unwrap(), DiffMode::Untracked);
        assert!(DiffMode::parse("HEAD~1").is_err());
        assert!(DiffMode::parse("").is_err());
    }

    #[test]
    fn synthesized_add_diff_counts_lines_and_marks_additions() {
        let d = synthesize_add_diff("src/new.rs", "fn main() {}\nlet x = 1;\n");
        assert!(d.contains("+++ b/src/new.rs"));
        assert!(d.contains("@@ -0,0 +1,2 @@"));
        assert!(d.contains("+fn main() {}\n"));
        assert!(d.contains("+let x = 1;\n"));
        assert!(!d.contains("No newline"));
    }

    #[test]
    fn synthesized_add_diff_flags_missing_trailing_newline() {
        let d = synthesize_add_diff("a.txt", "only line");
        assert!(d.contains("@@ -0,0 +1,1 @@"));
        assert!(d.contains("+only line\n"));
        assert!(d.ends_with("\\ No newline at end of file\n"));
    }

    #[test]
    fn synthesized_add_diff_for_empty_file_has_no_hunk() {
        let d = synthesize_add_diff("empty.txt", "");
        assert!(d.contains("new file"));
        assert!(!d.contains("@@"));
    }

    #[test]
    fn cap_keeps_small_diffs_untouched() {
        let d = "@@ -1,1 +1,1 @@\n-a\n+b\n".to_string();
        assert_eq!(cap_diff_text(d.clone()), d);
    }

    #[test]
    fn cap_truncates_on_line_boundary_with_notice() {
        // Build > 200 KiB of short lines.
        let line = "+xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
        let big: String = line.repeat((DIFF_CAP_BYTES / line.len()) + 50);
        let capped = cap_diff_text(big);
        assert!(capped.len() < DIFF_CAP_BYTES + 100);
        assert!(capped.ends_with("… diff truncated (first 200 KiB shown)\n"));
        // The kept portion must end on a full line (incl. newline) before the notice.
        let body = capped
            .strip_suffix("… diff truncated (first 200 KiB shown)\n")
            .unwrap();
        assert!(body.ends_with(line));
    }

    #[test]
    fn cap_truncates_giant_single_line_on_char_boundary() {
        // One multi-byte-char line larger than the cap, no newlines at all.
        let big = "é".repeat(DIFF_CAP_BYTES); // 2 bytes per char
        let capped = cap_diff_text(big);
        assert!(capped.ends_with("… diff truncated (first 200 KiB shown)\n"));
        // Must not have panicked on a char boundary and must be valid UTF-8
        // by construction (String), so just sanity-check the cut size.
        assert!(capped.len() <= DIFF_CAP_BYTES + 64);
    }

    #[test]
    fn guard_rejects_flag_after_separator() {
        let root = Path::new(".");
        assert!(guard_args(root, &["diff", "--no-color", "--", "-myfile"]).is_err());
        assert!(guard_args(root, &["diff", "--no-color", "--", "src/lib.rs"]).is_ok());
        assert!(guard_args(root, &["diff", "--cached", "--", "../escape"]).is_err());
    }

    #[test]
    fn containment_rejects_traversal_and_absolute() {
        let root = Path::new(".");
        assert!(ensure_within_root(root, "../escape").is_err());
        assert!(ensure_within_root(root, "a/../../escape").is_err());
        assert!(ensure_within_root(root, "/etc/passwd").is_err());
        assert!(ensure_within_root(root, "src/lib.rs").is_ok());
        assert!(ensure_within_root(root, "a/../b").is_ok());
    }
}
