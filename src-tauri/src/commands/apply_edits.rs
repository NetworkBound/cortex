//! Aider-style SEARCH/REPLACE edit-block application.
//!
//! Tool-capable adapters (the Cortex Gateway, the Claude CLI) apply file edits
//! themselves. Plain text-completion models — most importantly a **local
//! Ollama** model served via the Cookbook — have no tool channel, so when you
//! ask one to change code it can only *describe* the edit in its reply. Aider
//! solved this with a textual edit format the model emits and the client
//! applies:
//!
//! ```text
//! path/to/file.rs
//! <<<<<<< SEARCH
//! old lines, copied verbatim from the file
//! =======
//! the replacement lines
//! >>>>>>> REPLACE
//! ```
//!
//! This module parses those blocks out of an arbitrary assistant message and
//! applies them to files under the active project root. An empty SEARCH block
//! creates a new file. Matching is exact first, then falls back to a
//! trailing-whitespace-insensitive line match (handles CRLF / trailing-space
//! drift between what the model copied and what's on disk). Every write is
//! confined to the project root — a block naming an absolute path or escaping
//! via `..` is rejected, never applied.
//!
//! The parse + apply logic is pure and unit-tested end-to-end on real temp
//! repos; the Tauri command is a thin async wrapper.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// One parsed SEARCH/REPLACE block: which file, what to find, what to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditBlock {
    pub path: String,
    pub search: String,
    pub replace: String,
}

/// Outcome of attempting to apply a single block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockResult {
    pub path: String,
    /// `"applied"` (search matched + replaced), `"created"` (new file from an
    /// empty SEARCH), or `"failed"` (see `reason`).
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub search_lines: usize,
    pub replace_lines: usize,
}

/// Full report for an `apply_edit_blocks` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub results: Vec<BlockResult>,
    pub applied: usize,
    pub created: usize,
    pub failed: usize,
    /// True when called with `dry_run: true` — nothing was written to disk.
    pub dry_run: bool,
    /// Id of the workspace checkpoint taken immediately before writing, when at
    /// least one block actually changed a file (the Cline "checkpoint before
    /// agent edit" pattern). `None` on a dry-run, when nothing applied, or if the
    /// snapshot failed (which never blocks the edit). The UI uses it to offer a
    /// one-click undo via the Checkpoints panel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
}

// ---- marker recognition -------------------------------------------------

/// `<<<<<<< SEARCH` — at least five `<` then the word SEARCH (case-insensitive).
fn is_search_marker(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("<<<<<")
        && t.trim_start_matches('<').trim().to_ascii_uppercase().starts_with("SEARCH")
}

/// `=======` — a run of at least five `=` and nothing else.
fn is_divider(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 5 && t.chars().all(|c| c == '=')
}

/// `>>>>>>> REPLACE` — at least five `>` then the word REPLACE.
fn is_replace_marker(line: &str) -> bool {
    let t = line.trim();
    t.starts_with(">>>>>")
        && t.trim_start_matches('>').trim().to_ascii_uppercase().starts_with("REPLACE")
}

/// A code-fence line (```` ``` ```` optionally with an info string).
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Heuristic: does this line look like a file path the model put above a block?
/// Filenames carry a directory separator or an extension and never contain
/// internal whitespace — this keeps prose ("Here is the change:") from being
/// mistaken for a path. Surrounding backticks and a trailing colon are stripped
/// by the caller before this is checked.
fn looks_like_path(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return false;
    }
    if t.contains('/') {
        return true;
    }
    // An extension: a dot followed by 1..=8 alphanumerics at the very end.
    match t.rfind('.') {
        Some(i) if i + 1 < t.len() => {
            let ext = &t[i + 1..];
            (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        _ => false,
    }
}

/// Normalize a candidate path line: drop wrapping backticks and a trailing
/// colon so `` `src/app.py:` `` → `src/app.py`.
fn clean_path_candidate(line: &str) -> String {
    line.trim().trim_matches('`').trim().trim_end_matches(':').trim().to_string()
}

/// Parse every SEARCH/REPLACE block out of an arbitrary assistant message.
/// The filename for a block is the nearest preceding path-looking line (fences
/// and blank lines between the name and the `<<<<<<< SEARCH` marker are
/// skipped). A block whose filename can't be determined is dropped, since we
/// can't safely guess which file to touch.
pub fn parse_edit_blocks(text: &str) -> Vec<EditBlock> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut pending_path: Option<String> = None;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if is_search_marker(line) {
            // Collect SEARCH body until the divider.
            let mut search: Vec<&str> = Vec::new();
            let mut j = i + 1;
            let mut saw_divider = false;
            while j < lines.len() {
                if is_divider(lines[j]) {
                    saw_divider = true;
                    break;
                }
                search.push(lines[j]);
                j += 1;
            }
            if !saw_divider {
                break; // malformed / truncated block — stop.
            }
            // Collect REPLACE body until the replace marker.
            let mut replace: Vec<&str> = Vec::new();
            let mut k = j + 1;
            let mut saw_replace = false;
            while k < lines.len() {
                if is_replace_marker(lines[k]) {
                    saw_replace = true;
                    break;
                }
                replace.push(lines[k]);
                k += 1;
            }
            if !saw_replace {
                break; // truncated — don't risk a partial edit.
            }
            if let Some(path) = pending_path.take() {
                blocks.push(EditBlock {
                    path,
                    search: join_lines(&search),
                    replace: join_lines(&replace),
                });
            }
            i = k + 1;
            continue;
        }

        // Track the most recent path-looking line as the pending filename.
        if !is_fence(line) && !is_divider(line) && !is_replace_marker(line) {
            let cleaned = clean_path_candidate(line);
            if looks_like_path(&cleaned) {
                pending_path = Some(cleaned);
            }
        }
        i += 1;
    }
    blocks
}

/// Re-join collected lines, preserving a trailing newline only when there was
/// body content (so an empty SEARCH stays a truly empty string).
fn join_lines(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

// ---- path confinement ---------------------------------------------------

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a block's (relative) path against the project root, refusing
/// absolute paths and any `..` escape. Returns the absolute target path.
fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("empty path".into());
    }
    if rel.contains('\0') {
        return Err("path contains NUL".into());
    }
    let pb = PathBuf::from(rel);
    if pb.is_absolute() {
        return Err(format!("refusing absolute path in edit block: {rel}"));
    }
    let joined = lexical_normalize(&root.join(&pb));
    let root_norm = lexical_normalize(root);
    if !joined.starts_with(&root_norm) {
        return Err(format!("path escapes project root: {rel}"));
    }
    Ok(joined)
}

// ---- matching -----------------------------------------------------------

/// Find `search` in `haystack` and return the replaced full text, or `None` if
/// no match. Tries an exact substring match first, then a
/// trailing-whitespace-insensitive line-block match (handles CRLF / trailing
/// spaces the model dropped when copying).
fn apply_search_replace(haystack: &str, search: &str, replace: &str) -> Option<String> {
    // 1. Exact substring — replace only the first occurrence.
    if let Some(pos) = haystack.find(search) {
        let mut out = String::with_capacity(haystack.len() - search.len() + replace.len());
        out.push_str(&haystack[..pos]);
        out.push_str(replace);
        out.push_str(&haystack[pos + search.len()..]);
        return Some(out);
    }

    // 2. Trailing-whitespace-insensitive line-block match.
    let hay_lines: Vec<&str> = haystack.split('\n').collect();
    let search_trimmed = search.strip_suffix('\n').unwrap_or(search);
    let needle_lines: Vec<&str> = search_trimmed.split('\n').collect();
    if needle_lines.is_empty() {
        return None;
    }
    let n = needle_lines.len();
    if n > hay_lines.len() {
        return None;
    }
    for start in 0..=(hay_lines.len() - n) {
        let matches = (0..n).all(|off| {
            hay_lines[start + off].trim_end() == needle_lines[off].trim_end()
        });
        if matches {
            let mut rebuilt: Vec<String> = Vec::new();
            rebuilt.extend(hay_lines[..start].iter().map(|s| s.to_string()));
            let repl_trimmed = replace.strip_suffix('\n').unwrap_or(replace);
            if !repl_trimmed.is_empty() || !replace.is_empty() {
                rebuilt.extend(repl_trimmed.split('\n').map(|s| s.to_string()));
            }
            rebuilt.extend(hay_lines[start + n..].iter().map(|s| s.to_string()));
            return Some(rebuilt.join("\n"));
        }
    }
    None
}

/// Apply parsed blocks under `root`. Pure aside from filesystem writes; when
/// `dry_run` is set nothing is written but the would-be outcome is reported.
pub fn apply_blocks(root: &Path, blocks: &[EditBlock], dry_run: bool) -> ApplyReport {
    let mut results = Vec::with_capacity(blocks.len());
    let (mut applied, mut created, mut failed) = (0usize, 0usize, 0usize);

    for b in blocks {
        let search_lines = b.search.lines().count();
        let replace_lines = b.replace.lines().count();
        let push_fail = |reason: String, results: &mut Vec<BlockResult>, failed: &mut usize| {
            *failed += 1;
            results.push(BlockResult {
                path: b.path.clone(),
                status: "failed".into(),
                reason: Some(reason),
                search_lines,
                replace_lines,
            });
        };

        let target = match resolve_in_root(root, &b.path) {
            Ok(t) => t,
            Err(e) => {
                push_fail(e, &mut results, &mut failed);
                continue;
            }
        };

        // Empty SEARCH ⇒ create a new file.
        if b.search.trim().is_empty() {
            if target.exists() {
                push_fail(
                    "file already exists; an empty SEARCH only creates new files".into(),
                    &mut results,
                    &mut failed,
                );
                continue;
            }
            if !dry_run {
                if let Some(parent) = target.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        push_fail(format!("mkdir: {e}"), &mut results, &mut failed);
                        continue;
                    }
                }
                if let Err(e) = fs::write(&target, &b.replace) {
                    push_fail(format!("write: {e}"), &mut results, &mut failed);
                    continue;
                }
            }
            created += 1;
            results.push(BlockResult {
                path: b.path.clone(),
                status: "created".into(),
                reason: None,
                search_lines,
                replace_lines,
            });
            continue;
        }

        // Existing-file edit.
        let current = match fs::read_to_string(&target) {
            Ok(c) => c,
            Err(e) => {
                push_fail(format!("read: {e}"), &mut results, &mut failed);
                continue;
            }
        };
        match apply_search_replace(&current, &b.search, &b.replace) {
            Some(updated) => {
                if !dry_run {
                    if let Err(e) = fs::write(&target, &updated) {
                        push_fail(format!("write: {e}"), &mut results, &mut failed);
                        continue;
                    }
                }
                applied += 1;
                results.push(BlockResult {
                    path: b.path.clone(),
                    status: "applied".into(),
                    reason: None,
                    search_lines,
                    replace_lines,
                });
            }
            None => push_fail(
                "SEARCH text not found in file".into(),
                &mut results,
                &mut failed,
            ),
        }
    }

    ApplyReport {
        results,
        applied,
        created,
        failed,
        dry_run,
        checkpoint_id: None,
    }
}

/// Parse `text` for SEARCH/REPLACE blocks and apply them under `project_root`.
/// With `dry_run: true` the blocks are validated + matched but nothing is
/// written — for a preview before the user commits. Returns an error only when
/// the project root itself is unusable; per-block failures are reported inside
/// `ApplyReport`.
#[tauri::command]
pub async fn apply_edit_blocks(
    project_root: String,
    text: String,
    dry_run: Option<bool>,
) -> Result<ApplyReport, String> {
    tokio::task::spawn_blocking(move || {
        let root = PathBuf::from(&project_root);
        if !root.is_dir() {
            return Err(format!("not a directory: {project_root}"));
        }
        let blocks = parse_edit_blocks(&text);
        let is_dry = dry_run.unwrap_or(false);

        // Cline-style safety net: before mutating the user's files, snapshot the
        // workspace so the apply is undoable. We only checkpoint a *real* run
        // that would actually change something — a side-effect-free dry-run of
        // the same blocks tells us whether any file edits or creations land, so
        // a no-op apply (all blocks fail to match) doesn't spend a checkpoint.
        let checkpoint_id = if is_dry {
            None
        } else {
            let preview = apply_blocks(&root, &blocks, true);
            if preview.applied + preview.created > 0 {
                match crate::commands::checkpoints::make_checkpoint(
                    &root,
                    Some("before /apply".to_string()),
                ) {
                    Ok(info) => Some(info.id),
                    // A failed snapshot must never block the edit — surface it in
                    // the log and proceed with checkpoint_id: None.
                    Err(e) => {
                        tracing::warn!("apply_edit_blocks: pre-apply checkpoint failed: {e}");
                        None
                    }
                }
            } else {
                None
            }
        };

        let mut report = apply_blocks(&root, &blocks, is_dry);
        report.checkpoint_id = checkpoint_id;
        Ok(report)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_single_block_with_filename_line() {
        let text = "Here's the fix:\n\nsrc/app.rs\n<<<<<<< SEARCH\nfn old() {}\n=======\nfn new() {}\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].path, "src/app.rs");
        assert_eq!(blocks[0].search, "fn old() {}\n");
        assert_eq!(blocks[0].replace, "fn new() {}\n");
    }

    #[test]
    fn parses_fenced_block_skipping_the_fence_for_the_filename() {
        let text = "mathweb/flask/app.py\n```python\n<<<<<<< SEARCH\nx = 1\n=======\nx = 2\n>>>>>>> REPLACE\n```\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].path, "mathweb/flask/app.py");
        assert_eq!(blocks[0].search, "x = 1\n");
        assert_eq!(blocks[0].replace, "x = 2\n");
    }

    #[test]
    fn parses_multiple_blocks_across_files() {
        let text = "\
a.txt
<<<<<<< SEARCH
one
=======
ONE
>>>>>>> REPLACE

b.txt
<<<<<<< SEARCH
two
=======
TWO
>>>>>>> REPLACE
";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].path, "a.txt");
        assert_eq!(blocks[1].path, "b.txt");
        assert_eq!(blocks[1].replace, "TWO\n");
    }

    #[test]
    fn new_file_block_has_empty_search() {
        let text = "notes/new.md\n<<<<<<< SEARCH\n=======\n# Hello\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].search.is_empty());
        assert_eq!(blocks[0].replace, "# Hello\n");
    }

    #[test]
    fn block_without_a_filename_is_dropped() {
        // No path-looking line precedes the marker.
        let text = "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n";
        assert!(parse_edit_blocks(text).is_empty());
    }

    #[test]
    fn prose_is_not_mistaken_for_a_path() {
        let text = "Here is the change:\nsrc/x.rs\n<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].path, "src/x.rs");
    }

    #[test]
    fn truncated_block_is_not_emitted() {
        let text = "f.txt\n<<<<<<< SEARCH\na\n=======\nb\n"; // no REPLACE marker
        assert!(parse_edit_blocks(text).is_empty());
    }

    #[test]
    fn applies_exact_match() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("app.rs");
        fs::write(&f, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        let blocks = vec![EditBlock {
            path: "app.rs".into(),
            search: "    println!(\"hi\");\n".into(),
            replace: "    println!(\"bye\");\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.applied, 1);
        assert_eq!(report.failed, 0);
        let got = fs::read_to_string(&f).unwrap();
        assert!(got.contains("bye"));
        assert!(!got.contains("hi"));
    }

    #[test]
    fn applies_trailing_whitespace_insensitive_match() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("a.txt");
        // File has trailing spaces the model didn't copy.
        fs::write(&f, "alpha   \nbeta\n").unwrap();
        let blocks = vec![EditBlock {
            path: "a.txt".into(),
            search: "alpha\nbeta\n".into(),
            replace: "ALPHA\nBETA\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.applied, 1, "{:?}", report.results);
        assert_eq!(fs::read_to_string(&f).unwrap(), "ALPHA\nBETA\n");
    }

    #[test]
    fn search_not_found_fails_and_leaves_file_intact() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("a.txt");
        fs::write(&f, "original\n").unwrap();
        let blocks = vec![EditBlock {
            path: "a.txt".into(),
            search: "nonexistent\n".into(),
            replace: "x\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.failed, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(fs::read_to_string(&f).unwrap(), "original\n");
        assert!(report.results[0].reason.as_ref().unwrap().contains("not found"));
    }

    #[test]
    fn new_file_is_created() {
        let td = TempDir::new().unwrap();
        let blocks = vec![EditBlock {
            path: "sub/new.txt".into(),
            search: String::new(),
            replace: "fresh\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.created, 1);
        assert_eq!(fs::read_to_string(td.path().join("sub/new.txt")).unwrap(), "fresh\n");
    }

    #[test]
    fn empty_search_on_existing_file_fails() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("exists.txt");
        fs::write(&f, "keep\n").unwrap();
        let blocks = vec![EditBlock {
            path: "exists.txt".into(),
            search: String::new(),
            replace: "clobber\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.failed, 1);
        assert_eq!(fs::read_to_string(&f).unwrap(), "keep\n");
    }

    #[test]
    fn rejects_absolute_path_block() {
        let td = TempDir::new().unwrap();
        let blocks = vec![EditBlock {
            path: "/etc/passwd".into(),
            search: "root".into(),
            replace: "pwned".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.failed, 1);
        assert!(report.results[0].reason.as_ref().unwrap().contains("absolute"));
    }

    #[test]
    fn rejects_traversal_escape() {
        let td = TempDir::new().unwrap();
        let blocks = vec![EditBlock {
            path: "../../../etc/evil".into(),
            search: String::new(),
            replace: "x".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.failed, 1);
        assert!(report.results[0].reason.as_ref().unwrap().contains("escapes"));
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("a.txt");
        fs::write(&f, "before\n").unwrap();
        let blocks = vec![EditBlock {
            path: "a.txt".into(),
            search: "before\n".into(),
            replace: "after\n".into(),
        }];
        let report = apply_blocks(td.path(), &blocks, true);
        assert!(report.dry_run);
        assert_eq!(report.applied, 1);
        // File on disk is unchanged.
        assert_eq!(fs::read_to_string(&f).unwrap(), "before\n");
    }

    #[test]
    fn mixed_results_count_correctly() {
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("hit.txt"), "find\n").unwrap();
        fs::write(td.path().join("miss.txt"), "other\n").unwrap();
        let blocks = vec![
            EditBlock { path: "hit.txt".into(), search: "find\n".into(), replace: "FOUND\n".into() },
            EditBlock { path: "miss.txt".into(), search: "absent\n".into(), replace: "x\n".into() },
            EditBlock { path: "made.txt".into(), search: String::new(), replace: "new\n".into() },
        ];
        let report = apply_blocks(td.path(), &blocks, false);
        assert_eq!(report.applied, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.created, 1);
    }

    #[test]
    fn end_to_end_command_parses_and_applies() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("greet.py");
        fs::write(&f, "def hi():\n    return 'hi'\n").unwrap();
        let text = "Let's fix it:\n\ngreet.py\n```python\n<<<<<<< SEARCH\n    return 'hi'\n=======\n    return 'hello'\n>>>>>>> REPLACE\n```\n";
        let report = tauri::async_runtime::block_on(apply_edit_blocks(
            td.path().display().to_string(),
            text.to_string(),
            None,
        ))
        .unwrap();
        assert_eq!(report.applied, 1, "{:?}", report.results);
        assert_eq!(fs::read_to_string(&f).unwrap(), "def hi():\n    return 'hello'\n");
    }

    #[test]
    fn command_rejects_non_directory_root() {
        let err = tauri::async_runtime::block_on(apply_edit_blocks(
            "/no/such/dir/xyz".into(),
            "anything".into(),
            None,
        ))
        .unwrap_err();
        assert!(err.contains("not a directory"));
    }

    /// Count `.tar.gz` snapshots in a project's checkpoints dir (0 if none).
    fn checkpoint_count(root: &std::path::Path) -> usize {
        let dir = root.join(".cortex").join("checkpoints");
        match fs::read_dir(&dir) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tar.gz"))
                .count(),
            Err(_) => 0,
        }
    }

    #[test]
    fn real_apply_takes_a_checkpoint_first() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("greet.py");
        fs::write(&f, "def hi():\n    return 'hi'\n").unwrap();
        assert_eq!(checkpoint_count(td.path()), 0, "no snapshots before apply");
        let text = "greet.py\n<<<<<<< SEARCH\n    return 'hi'\n=======\n    return 'hello'\n>>>>>>> REPLACE\n";
        let report = tauri::async_runtime::block_on(apply_edit_blocks(
            td.path().display().to_string(),
            text.to_string(),
            None,
        ))
        .unwrap();
        // The edit landed, a checkpoint id came back, and a tarball exists on disk
        // — so the apply is undoable.
        assert_eq!(report.applied, 1);
        let id = report.checkpoint_id.expect("real apply should snapshot first");
        assert!(td.path().join(".cortex").join("checkpoints").join(format!("{id}.tar.gz")).exists());
        assert_eq!(checkpoint_count(td.path()), 1);
    }

    #[test]
    fn dry_run_does_not_checkpoint() {
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("greet.py"), "x = 1\n").unwrap();
        let text = "greet.py\n<<<<<<< SEARCH\nx = 1\n=======\nx = 2\n>>>>>>> REPLACE\n";
        let report = tauri::async_runtime::block_on(apply_edit_blocks(
            td.path().display().to_string(),
            text.to_string(),
            Some(true),
        ))
        .unwrap();
        assert!(report.dry_run);
        assert!(report.checkpoint_id.is_none(), "a preview must not snapshot");
        assert_eq!(checkpoint_count(td.path()), 0);
    }

    #[test]
    fn no_op_apply_does_not_checkpoint() {
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("greet.py"), "real\n").unwrap();
        // SEARCH text that isn't in the file → the only block fails, nothing changes.
        let text = "greet.py\n<<<<<<< SEARCH\nnot present\n=======\nx\n>>>>>>> REPLACE\n";
        let report = tauri::async_runtime::block_on(apply_edit_blocks(
            td.path().display().to_string(),
            text.to_string(),
            None,
        ))
        .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.applied + report.created, 0);
        assert!(report.checkpoint_id.is_none(), "a no-op apply must not spend a checkpoint");
        assert_eq!(checkpoint_count(td.path()), 0);
    }
}
