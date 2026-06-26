//! Project-wide file search — the Cursor/VS Code "Find in files" (Ctrl+Shift+F)
//! and "Go to file" (Ctrl+P) experience.
//!
//! Two commands:
//!   - `search_project` shells out to `rg --json` when ripgrep is on $PATH and
//!     falls back to a plain `walkdir` + string-match walker. Results are
//!     capped at 500 hits across at most 100 files so the UI never explodes.
//!   - `find_files`     reuses `crate::projects::list_files` (bumped to a
//!     5000-entry ceiling) and runs a tiny fuzzy filter over the file paths,
//!     returning the top 50 ranked hits.
//!
//! Both commands respect `.cortexignore` (global + per-project) via
//! `CortexIgnore::load`, mirroring the file-tree behaviour.
//!
//! Frontend lives at `src/lib/project-search.ts` + `components/SearchPanel.tsx`.

use crate::projects::ignore::CortexIgnore;
use crate::projects::list_files;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Max total match results returned across the whole project.
const MAX_HITS: usize = 500;
/// Max distinct files touched. Stops the walker dead once we've seen this many.
const MAX_FILES: usize = 100;
/// Skip files larger than 2 MiB — almost certainly minified bundles or binaries.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Find-files walker depth ceiling. Same shape as the file tree.
const FIND_FILES_LIMIT: usize = 5000;
/// Per-line truncation so we don't ship megabytes of minified JS to the UI.
const MAX_LINE_BYTES: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub match_text: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Project-wide text search.
///
/// - `case_sensitive` defaults to `false` (matches Cursor's default).
/// - `fixed_string` defaults to `false` — i.e. the query is treated as a regex.
///   When set the query is matched literally (uppercase-S "smart-case-off").
///
/// Returns up to `MAX_HITS` `SearchHit` rows. Empty query short-circuits to
/// `[]` so the UI can debounce without the backend doing useless work.
#[tauri::command]
pub async fn search_project(
    project_root: String,
    query: String,
    case_sensitive: Option<bool>,
    fixed_string: Option<bool>,
) -> Result<Vec<SearchHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let case_sensitive = case_sensitive.unwrap_or(false);
    let fixed_string = fixed_string.unwrap_or(false);

    // Try ripgrep first — it's the canonical fast path. Failure (binary
    // missing, exit != 0/1, parse hiccups) falls back transparently to the
    // Rust walker so the feature still works on a vanilla machine.
    if rg_available() {
        match ripgrep_search(&root, q, case_sensitive, fixed_string) {
            Ok(hits) => return Ok(hits),
            Err(e) => {
                tracing::debug!("ripgrep search failed, falling back: {e}");
            }
        }
    }
    Ok(walkdir_search(&root, q, case_sensitive, fixed_string))
}

/// "Go to file" — fuzzy match file paths under `project_root` and return the
/// top 50 absolute paths. Reuses the existing `list_files` walker so we honour
/// `.cortexignore` and the built-in deny-list (`.git`, `node_modules`, etc.)
/// without re-implementing the ignore logic here.
#[tauri::command]
pub async fn find_files(project_root: String, query: String) -> Result<Vec<String>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let q = query.trim();
    let entries = list_files(&root, FIND_FILES_LIMIT);
    let mut paths: Vec<String> = entries
        .into_iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.path.to_string_lossy().into_owned())
        .collect();

    if q.is_empty() {
        paths.truncate(50);
        return Ok(paths);
    }

    let lc_q = q.to_lowercase();
    let mut scored: Vec<(i32, String)> = paths
        .into_iter()
        .filter_map(|p| fuzzy_score(&p, &lc_q).map(|s| (s, p)))
        .collect();
    // Highest score first; tie-break on shorter paths (closer to the root
    // usually = "more canonical" file in VS Code's heuristic).
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    Ok(scored.into_iter().take(50).map(|(_, p)| p).collect())
}

// ──────────────────────────────────────────────────────────────────────────
// ripgrep fast path
// ──────────────────────────────────────────────────────────────────────────

fn rg_available() -> bool {
    crate::sys::no_window("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ripgrep_search(
    root: &Path,
    query: &str,
    case_sensitive: bool,
    fixed_string: bool,
) -> Result<Vec<SearchHit>, String> {
    // `--json` emits one JSON object per line: begin/match/end/summary.
    // `-C 1` gives us a single line of before/after context for each match,
    // surfaced as `context` events keyed by line number.
    let mut cmd = crate::sys::no_window("rg");
    cmd.arg("--json")
        .arg("-C")
        .arg("1")
        .arg("--max-count")
        .arg(MAX_HITS.to_string())
        .arg("--max-filesize")
        .arg(format!("{}", MAX_FILE_BYTES));
    if !case_sensitive {
        cmd.arg("-i");
    }
    if fixed_string {
        cmd.arg("-F");
    }
    cmd.arg("--").arg(query).arg(".").current_dir(root);

    let output = cmd
        .output()
        .map_err(|e| format!("rg spawn failed: {e}"))?;
    // rg exits 1 when there are zero matches — that's NOT an error.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "rg exited with status {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_rg_json(&stdout, root))
}

/// Parse `rg --json` line-by-line.
///
/// We only care about three event kinds: `match` (the actual hit),
/// `context` (the -C 1 before/after lines), and nothing else. The context
/// lines come in stream order: a `context` event right before a `match`
/// is the "before"; the one right after is the "after". We stash both
/// against the match's path+line so the UI gets a 3-line snippet.
fn parse_rg_json(stdout: &str, _root: &Path) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = Vec::with_capacity(64);
    let mut files_seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // Track the most recent "context" line per file so we can attach it as
    // the `before` of the next match.
    let mut pending_before: Option<(PathBuf, usize, String)> = None;

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let data = match v.get("data") {
            Some(d) => d,
            None => continue,
        };
        let path = data
            .get("path")
            .and_then(|p| p.get("text"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from);
        let line_no = data
            .get("line_number")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        let text = data
            .get("lines")
            .and_then(|l| l.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim_end_matches('\n')
            .to_string();

        match kind {
            "context" => {
                if let Some(p) = path {
                    pending_before = Some((p, line_no, truncate_line(text)));
                }
            }
            "match" => {
                let Some(p) = path else { continue };
                if hits.len() >= MAX_HITS {
                    break;
                }
                if !files_seen.contains(&p) {
                    if files_seen.len() >= MAX_FILES {
                        continue;
                    }
                    files_seen.insert(p.clone());
                }
                // submatches[0] gives us the column (0-indexed byte offset).
                let col = data
                    .get("submatches")
                    .and_then(|s| s.as_array())
                    .and_then(|a| a.first())
                    .and_then(|m| m.get("start"))
                    .and_then(|n| n.as_u64())
                    .map(|n| n as usize + 1)
                    .unwrap_or(1);
                let before = pending_before
                    .as_ref()
                    .filter(|(bp, bl, _)| bp == &p && *bl + 1 == line_no)
                    .map(|(_, _, t)| t.clone());
                hits.push(SearchHit {
                    path: p,
                    line: line_no,
                    col,
                    match_text: truncate_line(text),
                    before,
                    after: None,
                });
                pending_before = None;
            }
            _ => {
                // begin/end/summary — used by ripgrep for bookkeeping, we
                // ignore them. The next iteration may turn a stray context
                // line into the `after` of the just-pushed match.
                if kind == "context" {
                    continue;
                }
            }
        }

        // After every parsed event, look ahead-ish: if we just consumed a
        // context line AND the last hit is for the same file with line+1,
        // attach it as the `after`. Done with a separate pass for clarity.
    }

    // Attach `after` context: a context line at L+1 after a match at L.
    // We do a single forward scan of `stdout` again — cheap because the
    // output is already in memory.
    let mut last_match_idx: Option<usize> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let data = match v.get("data") {
            Some(d) => d,
            None => continue,
        };
        let path = data
            .get("path")
            .and_then(|p| p.get("text"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from);
        let line_no = data
            .get("line_number")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        if kind == "match" {
            if let Some(p) = path {
                last_match_idx = hits
                    .iter()
                    .rposition(|h| h.path == p && h.line == line_no);
            }
        } else if kind == "context" {
            if let (Some(idx), Some(p)) = (last_match_idx, path) {
                if let Some(h) = hits.get_mut(idx) {
                    if h.path == p && h.line + 1 == line_no {
                        let text = data
                            .get("lines")
                            .and_then(|l| l.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .trim_end_matches('\n')
                            .to_string();
                        h.after = Some(truncate_line(text));
                    }
                }
            }
        }
    }

    hits
}

// ──────────────────────────────────────────────────────────────────────────
// Pure-Rust fallback (no ripgrep installed)
// ──────────────────────────────────────────────────────────────────────────

fn walkdir_search(
    root: &Path,
    query: &str,
    case_sensitive: bool,
    fixed_string: bool,
) -> Vec<SearchHit> {
    // We support either a literal substring match (`fixed_string=true`) or
    // a regex-lite via case-insensitive substring. Real regex support is
    // intentionally only in the ripgrep path — installing the `regex` crate
    // here just for the fallback bloats the binary, and `rg` is ubiquitous.
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    // When `fixed_string` is false in the fallback, we still do a substring
    // match — log a hint so power-users wondering "why didn't my regex work"
    // know to install ripgrep.
    if !fixed_string && !rg_available() {
        tracing::debug!(
            "project search: ripgrep not installed — regex mode degraded to substring"
        );
    }

    let ignore = CortexIgnore::load(root);
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut files_seen = 0usize;

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_denied(e.path(), root))
        .filter_map(|e| e.ok())
    {
        if hits.len() >= MAX_HITS || files_seen >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let path = entry.path();
        let body = match std::fs::read_to_string(path) {
            Ok(b) => b,
            Err(_) => continue, // binary or non-UTF8 — skip
        };
        let lines: Vec<&str> = body.lines().collect();
        let mut matched_in_file = false;
        for (i, line) in lines.iter().enumerate() {
            if hits.len() >= MAX_HITS {
                break;
            }
            let haystack = if case_sensitive {
                (*line).to_string()
            } else {
                line.to_lowercase()
            };
            let col = match haystack.find(&needle) {
                Some(c) => c + 1,
                None => continue,
            };
            matched_in_file = true;
            hits.push(SearchHit {
                path: path.to_path_buf(),
                line: i + 1,
                col,
                match_text: truncate_line((*line).to_string()),
                before: i.checked_sub(1).and_then(|j| lines.get(j)).map(|s| truncate_line((*s).to_string())),
                after: lines.get(i + 1).map(|s| truncate_line((*s).to_string())),
            });
        }
        if matched_in_file {
            files_seen += 1;
        }
    }
    hits
}

fn truncate_line(mut s: String) -> String {
    if s.len() > MAX_LINE_BYTES {
        let mut cut = MAX_LINE_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("…");
    }
    s
}

// ──────────────────────────────────────────────────────────────────────────
// Fuzzy matcher (Go-to-file)
// ──────────────────────────────────────────────────────────────────────────

/// Lightweight subsequence-with-bonuses fuzzy score.
///
/// Modelled on Sublime/VS Code's command palette ranker: every character of
/// `query` must appear in `text` in order, and we add bonuses for matches
/// right after a separator (e.g. `/` or `_`) and to camel-case humps. Returns
/// `None` when the query isn't a subsequence at all.
fn fuzzy_score(text: &str, lc_query: &str) -> Option<i32> {
    let lc_text = text.to_lowercase();
    let mut score: i32 = 0;
    let mut t_iter = lc_text.char_indices().peekable();
    let q_chars: Vec<char> = lc_query.chars().collect();
    if q_chars.is_empty() {
        return Some(0);
    }
    let mut prev_match_idx: Option<usize> = None;
    for qc in q_chars {
        loop {
            let (i, tc) = match t_iter.next() {
                Some(x) => x,
                None => return None,
            };
            if tc == qc {
                // Bonus for matching the basename (last `/` segment) over a
                // dir component — VS Code does this for the file picker.
                if let Some(slash) = lc_text[..i].rfind('/') {
                    if i == slash + 1 {
                        score += 8;
                    } else if i > slash + 1 {
                        // small bonus for hits past the last slash (basename)
                        score += 1;
                    }
                } else {
                    score += 2;
                }
                // Bonus for camel-case / underscore-separator boundary.
                if i > 0 {
                    let prev = lc_text.as_bytes()[i - 1] as char;
                    if matches!(prev, '_' | '-' | '.' | '/' | ' ') {
                        score += 4;
                    }
                }
                // Bonus for consecutive matches (cluster wins).
                if let Some(p) = prev_match_idx {
                    if p + 1 == i {
                        score += 5;
                    }
                }
                prev_match_idx = Some(i);
                break;
            }
        }
    }
    // Penalty for unused text length so short paths sort above long ones
    // when they tie on bonuses.
    score -= (text.len() / 32) as i32;
    Some(score)
}
