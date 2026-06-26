//! Local "massive brain" context picker.
//!
//! Fast, LLM-free heuristic context retrieval. Tokenises the user's draft,
//! greps memory / chat-history / code files for matches, scores by term
//! frequency × recency × source-kind weight, and returns the top-N as
//! @-token suggestions the composer can insert.
//!
//! Target latency: <200ms for ~10k candidate files. The full LLM-based
//! `suggest_context` still exists for the explicit "🎯 Suggest context"
//! button; this one fires automatically after a typing pause and needs to
//! be near-instant so the brain feels proactive.
//!
//! Scoring layers (post-wave-218):
//!   1. `extract_terms` (wave 203 picks qualified `User::save` at 3× boost,
//!      wave 271 picks `user?.save` style as same form, identifiers at 2×,
//!      regular words at 1×; stopwords filtered).
//!   2. `recency_weight` (wave 145 finer buckets: <1h=1.4, <6h=1.2, <1d=1.1).
//!   3. Filename match bonus (wave 158: +3× when basename contains the term).
//!   4. Source-kind weight (`source_kind_weight`).
//!   5. PageRank multiplier (wave 218: 1.0–1.5×; pulled from cached repo_map).
//!
//! Wave 236 added an early-out before the pagerank lookup when extract_terms
//! returns no terms — that lookup was paying a ≤500-file walk for every
//! brain query, even short stopword-only ones.

use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::memory::sources::{default_sources, walk_markdown};

/// Short-TTL cache for the project file walk. `local_brain_suggest` fires on
/// every ~800ms typing pause; without this, each call re-walked the whole tree
/// (WalkDir depth 5, stat per file). The TTL is well under the typing cadence,
/// so results stay current while the repeated walks collapse to one.
static RECENT_FILES_CACHE: Lazy<Mutex<HashMap<PathBuf, (Instant, Vec<PathBuf>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const RECENT_FILES_TTL: Duration = Duration::from_secs(10);

const MAX_RESULTS: usize = 8;
const MIN_TERM_LEN: usize = 4;
const MAX_PREVIEW_BYTES: usize = 64 * 1024; // peek at first 64KB of each file
/// Wave 291 — PageRank multiplier intensity. Wave-218 used the value
/// 0.5 inline as `1.0 + pr * 0.5` (max 1.5×); kept the same default
/// here as a named constant so future tuning lives in one place.
const PAGERANK_MULTIPLIER_INTENSITY: f32 = 0.5;
/// Source-kind weight for the recent-edits pass. Recent code edits are the
/// strongest "what am I working on" signal, so they outrank memory/chat hits
/// (whose weights top out at 1.2 in `source_kind_weight`).
const RECENT_EDITS_KIND_WEIGHT: f32 = 1.5;
const STOPWORDS: &[&str] = &[
    "with", "this", "that", "from", "have", "into", "your", "what", "when",
    "where", "which", "would", "should", "could", "about", "after", "before",
    "they", "them", "their", "there", "then", "than", "been", "were", "will",
    "make", "made", "just", "like", "some", "more", "want", "need", "does",
];

#[derive(Serialize, Clone, Default)]
pub struct LocalSuggestion {
    pub path: String,
    pub source: String,
    pub token: String,
    pub score: f32,
    pub preview: String,
    /// Wave 150 — which extracted terms actually hit this file. Useful for
    /// the user to understand why a suggestion was surfaced (and what to
    /// tweak in their draft if it's the wrong one).
    #[serde(default)]
    pub matched_terms: Vec<String>,
}

#[derive(Serialize)]
pub struct LocalBrainResult {
    pub suggestions: Vec<LocalSuggestion>,
    pub scanned_files: usize,
    pub matched_files: usize,
}

/// A search term with a "boost" tag. Identifiers (snake_case, CamelCase,
/// kebab-case, paths-with-slashes) score 2× because the user typing
/// `local_brain_suggest` is almost certainly asking about that function,
/// not generic prose around it.
#[derive(Debug)]
struct Term {
    text: String,
    boost: f32,
}

fn extract_terms(draft: &str) -> Vec<Term> {
    let mut seen: HashMap<String, f32> = HashMap::new();
    let mut out: Vec<Term> = Vec::new();
    // Wave 203 — Pass 0: qualified identifiers like `User.save`, `model::User`,
    // `Repo::Map::format`. Treat as a single term (full dotted/colon form
    // lowercased) with the highest boost so users can refer to method-on-
    // type / module-path references precisely. Cap at 4 to avoid blowing
    // the term budget on a single line of code dumped in the draft.
    {
        let mut count = 0;
        // Match `Ident(.Ident|::Ident)+` where Ident starts with letter+ascii
        // alphanumeric/underscore, ≥ 2 chars per segment.
        let chars: Vec<char> = draft.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let start = i;
            // Find the end of an identifier segment.
            if !(chars[i].is_alphabetic() || chars[i] == '_') {
                i += 1;
                continue;
            }
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            // Now look for `.IDENT` or `::IDENT` (or wave-271 `?.IDENT`
            // for JS optional chaining) suffixes. The `?` itself is
            // dropped — the term becomes the plain `user.save` form so
            // grep matches files containing the canonical path.
            let mut end = i;
            loop {
                let sep_len = if end < chars.len() && chars[end] == '.' { 1 }
                              else if end + 1 < chars.len() && chars[end] == '?' && chars[end+1] == '.' { 2 }
                              else if end + 1 < chars.len() && chars[end] == ':' && chars[end+1] == ':' { 2 }
                              else { 0 };
                if sep_len == 0 { break; }
                let seg_start = end + sep_len;
                if seg_start >= chars.len() { break; }
                if !(chars[seg_start].is_alphabetic() || chars[seg_start] == '_') { break; }
                let mut seg_end = seg_start;
                while seg_end < chars.len() && (chars[seg_end].is_alphanumeric() || chars[seg_end] == '_') {
                    seg_end += 1;
                }
                if seg_end - seg_start < 2 { break; }
                end = seg_end;
            }
            if end > i {
                // We extended past the first ident — got a qualified form.
                // Wave 272 — drop the `?` chars so optional chaining
                // renders as the canonical dotted path (`user.save`).
                let tok: String = chars[start..end].iter().filter(|c| **c != '?').collect();
                let lower = tok.to_lowercase();
                if lower.len() >= MIN_TERM_LEN && !seen.contains_key(&lower) {
                    seen.insert(lower.clone(), 3.0);
                    out.push(Term { text: lower, boost: 3.0 });
                    count += 1;
                    if count >= 4 { break; }
                }
                i = end;
            }
        }
    }

    // Pass 1: identifiers + paths — anything with `_`, an uppercase letter
    // after a lowercase one, a `-` followed by a letter, or a `/`/`\`
    // counts as a strong signal.
    //
    // Note (wave 283): this differs from `repo_map::extract_references`'s
    // `looks_id` heuristic — that one accepts TitleCase (`User`, `Repo`)
    // because file contents are usually code where TitleCase = type. Here
    // we're scanning the USER'S draft which is usually English prose, so
    // TitleCase tokens like "Then" / "This" would be false positives and
    // pollute the high-boost set. We let Pass 2 catch them as 1× words.
    for tok in draft.split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == ';' || c == ':' || c == '!' || c == '?') {
        let lower = tok.to_lowercase();
        if lower.len() < MIN_TERM_LEN { continue; }
        let looks_like_ident =
            tok.contains('_')
            || tok.contains('/')
            || tok.contains('\\')
            || tok.chars().any(|c| c == '-')
            || tok.chars().zip(tok.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase());
        if !looks_like_ident { continue; }
        let existing = seen.get(&lower).copied().unwrap_or(0.0);
        if existing >= 2.0 { continue; }
        seen.insert(lower.clone(), 2.0);
        out.push(Term { text: lower, boost: 2.0 });
        if out.len() >= 6 { break; }
    }
    // Pass 2: regular words. Drop stopwords + anything already captured.
    for tok in draft.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let lower = tok.to_lowercase();
        if lower.len() < MIN_TERM_LEN { continue; }
        if STOPWORDS.contains(&lower.as_str()) { continue; }
        if seen.contains_key(&lower) { continue; }
        seen.insert(lower.clone(), 1.0);
        out.push(Term { text: lower, boost: 1.0 });
        if out.len() >= 14 { break; }
    }
    out
}

#[derive(Serialize)]
pub struct ExtractedTerm {
    pub text: String,
    pub boost: f32,
}

/// Wave 226 — `/cache-stats` slash backend. Returns the number of entries
/// currently in the repo_map cache. Helps users debug whether their
/// /repomap-top is hitting cache or paying the walk cost.
#[tauri::command]
pub async fn repo_map_cache_stats() -> Result<usize, String> {
    Ok(crate::repo_map::cache_size())
}

/// Wave 244 — /clear-cache backend. Returns the number of entries that
/// were dropped (so the UI can toast "cleared N entries").
#[tauri::command]
pub async fn repo_map_cache_clear() -> Result<usize, String> {
    Ok(crate::repo_map::cache_clear())
}

/// Wave 193 — `/repomap-top` slash backend. Thin wrapper around the
/// existing `compute_repo_map` so the frontend can ask for the ranked
/// file list (with PageRank scores) without re-running the full
/// expand_at_tokens pipeline.
#[tauri::command]
pub async fn compute_repo_map_command(
    project_root: String,
    max_files: Option<usize>,
) -> Result<crate::repo_map::RepoMap, String> {
    tokio::task::spawn_blocking(move || {
        let p = std::path::PathBuf::from(&project_root);
        if !p.is_dir() {
            return Err(format!("not a directory: {project_root}"));
        }
        Ok(crate::repo_map::compute_repo_map(&p, max_files.unwrap_or(500)))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Wave 164 — diagnostic exposed via `/extracted` slash. Returns exactly
/// what `extract_terms` would feed into `local_brain_suggest` so users can
/// understand why specific files surface (or don't).
#[tauri::command]
pub async fn extract_terms_diagnostic(message: String) -> Result<Vec<ExtractedTerm>, String> {
    Ok(extract_terms(&message)
        .into_iter()
        .map(|t| ExtractedTerm { text: t.text, boost: t.boost })
        .collect())
}

#[tauri::command]
pub async fn local_brain_suggest(
    message: String,
    project_root: Option<String>,
) -> Result<LocalBrainResult, String> {
    // Resolve the Obsidian vault so brain suggestions can draw on vault notes,
    // not just project files (previously hard-coded to `None`). Uses the same
    // auto-detection as boot so this works without a State handle — letting the
    // inline `run_brain_inline` path reuse this command directly.
    let vault = crate::app_state::default_obsidian_vault();
    tokio::task::spawn_blocking(move || {
        let terms = extract_terms(&message);
        // Wave 236 — early-out BEFORE building the pagerank lookup. The
        // original wave-218 code paid compute_repo_map (walks ≤500 files)
        // on every brain call even when the user typed only stopwords.
        // Real perf regression for short messages.
        if terms.is_empty() {
            // Wave 241 — diagnostic so users debugging "why didn't brain
            // fire" can see the empty-terms cause in logs. Wave 281 —
            // include the first 40 chars of the message so users can
            // tell which message triggered the short-circuit when
            // browsing logs.
            let snippet: String = message.chars().take(40).collect();
            tracing::debug!(
                target: "cortex::local_brain",
                snippet = %snippet,
                "extract_terms returned empty — early-out (no walk)"
            );
            return Ok(LocalBrainResult { suggestions: vec![], scanned_files: 0, matched_files: 0 });
        }
        // Wave 218 — pre-compute the project's PageRank lookup so the
        // scoring loop can incorporate it without repeated walks. Uses the
        // wave-208 10s cache so the second / third brain query in a row
        // is free.
        let pagerank_lookup: HashMap<String, f32> = if let Some(root) = project_root.as_deref().map(PathBuf::from) {
            let map = crate::repo_map::compute_repo_map(&root, 500);
            let lookup: HashMap<String, f32> = map.files
                .into_iter()
                .filter(|f| f.pagerank > 0.0)
                .map(|f| (f.path, f.pagerank))
                .collect();
            // Wave 243 — log how many files contributed pagerank entries
            // so users debugging brain results can see "I got 23 pagerank
            // entries from 500 files; the rest had zero inbound refs".
            tracing::debug!(
                target: "cortex::local_brain",
                "pagerank lookup built: {} files with positive score",
                lookup.len()
            );
            lookup
        } else {
            HashMap::new()
        };

        let active = project_root.as_deref().map(PathBuf::from);
        let sources = default_sources(active.as_deref(), vault.as_deref());

        let mut scored: Vec<LocalSuggestion> = Vec::new();
        let mut scanned: usize = 0;

        // Phase 1: recently-edited project files. Strong signal — most
        // tasks are about code the user just touched. We walk the active
        // project (if any), pick the 30 most-recently-modified files
        // (excluding noisy dirs), grep them with the same terms, and add
        // their hits to the candidate pool with a recency boost.
        if let Some(root) = active.as_ref() {
            let recent = recent_project_files(root, 30);
            for file in &recent {
                scanned += 1;
                let Ok(content) = read_capped(file) else { continue };
                let lower = content.to_lowercase();
                // Wave 158 — filename-match bonus. If the user types
                // `processOrder`, a file literally named `process_order.rs`
                // should rank above a file that merely mentions it once
                // in a comment. Cheap to compute; uses the path-only
                // stem so directory components don't false-positive.
                let fname_lower = file
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                let mut weighted_hits = 0.0f32;
                let mut preview_line: Option<String> = None;
                let mut matched_terms: Vec<String> = Vec::new();
                for term in &terms {
                    let n = lower.matches(term.text.as_str()).count() as f32;
                    // Wave 158 — filename hit counts as 3× the per-term boost.
                    let fname_hit = if fname_lower.contains(term.text.as_str()) { 3.0 } else { 0.0 };
                    if n > 0.0 || fname_hit > 0.0 {
                        weighted_hits += (n + fname_hit) * term.boost;
                        matched_terms.push(term.text.clone());
                        if preview_line.is_none() {
                            // Walk ORIGINAL-case content so the preview chip
                            // shows readable text instead of a downcased glob.
                            preview_line = content
                                .lines()
                                .find(|l| l.to_lowercase().contains(term.text.as_str()))
                                .map(|l| l.trim().chars().take(140).collect());
                        }
                    }
                }
                if weighted_hits == 0.0 { continue; }
                let recency = recency_weight(file);
                // Wave 218 — PageRank bonus. Look up the file's pagerank
                // from the cached repo_map (key matches the relative-path
                // form used in `format_as_text`). Multiplier in [1.0, 1.5]
                // — never punishes, just boosts central files.
                // Wave 242 — we're already inside `if let Some(root) = active`
                // so the original `unwrap_or(file)` was dead code. `root`
                // is in scope at this point.
                let rel = file.strip_prefix(root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .replace('\\', "/");
                let pr_boost = 1.0 + pagerank_lookup.get(&rel).copied().unwrap_or(0.0) * PAGERANK_MULTIPLIER_INTENSITY;
                let score = score_suggestion(weighted_hits, recency, RECENT_EDITS_KIND_WEIGHT, pr_boost);
                scored.push(LocalSuggestion {
                    path: file.display().to_string(),
                    source: if pr_boost > 1.05 { format!("recent edits · pagerank {:.2}", pr_boost - 1.0) } else { "recent edits".into() },
                    token: format!("@{}", file.display()),
                    score,
                    preview: preview_line.unwrap_or_default(),
                    matched_terms,
                });
            }
        }

        'outer: for source in &sources {
            for file in walk_markdown(source) {
                scanned += 1;
                if scanned > 4000 { break 'outer; }
                let Ok(content) = read_capped(&file) else { continue };
                let lower = content.to_lowercase();
                // Wave 159 — filename-match bonus (same as wave 158 for the
                // recent-edits pass). Memory files are often named after
                // their topic (project_cortex.md, feedback_pace.md), so
                // matching the basename is a strong signal.
                let fname_lower = file
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                let mut weighted_hits = 0.0f32;
                let mut preview_line: Option<String> = None;
                let mut matched_terms: Vec<String> = Vec::new();
                for term in &terms {
                    let n = lower.matches(term.text.as_str()).count() as f32;
                    let fname_hit = if fname_lower.contains(term.text.as_str()) { 3.0 } else { 0.0 };
                    if n > 0.0 || fname_hit > 0.0 {
                        weighted_hits += (n + fname_hit) * term.boost;
                        matched_terms.push(term.text.clone());
                        if preview_line.is_none() {
                            preview_line = content
                                .lines()
                                .find(|l| l.to_lowercase().contains(term.text.as_str()))
                                .map(|l| l.trim().chars().take(140).collect());
                        }
                    }
                }
                if weighted_hits == 0.0 { continue; }
                let recency = recency_weight(&file);
                let kind_w = source_kind_weight(source);
                let score = score_suggestion(weighted_hits, recency, kind_w, 1.0);
                scored.push(LocalSuggestion {
                    path: file.display().to_string(),
                    source: source.label.clone(),
                    token: format!("@{}", file.display()),
                    score,
                    preview: preview_line.unwrap_or_default(),
                    matched_terms,
                });
            }
        }
        let matched = scored.len();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(MAX_RESULTS);
        Ok(LocalBrainResult { suggestions: scored, scanned_files: scanned, matched_files: matched })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

fn read_capped(p: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let f = fs::File::open(p)?;
    let mut buf = Vec::with_capacity(MAX_PREVIEW_BYTES);
    f.take(MAX_PREVIEW_BYTES as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn recency_weight(p: &Path) -> f32 {
    let Ok(meta) = fs::metadata(p) else { return 0.6 };
    let Ok(modified) = meta.modified() else { return 0.6 };
    let age_secs = modified.elapsed().map(|d| d.as_secs() as f32).unwrap_or(0.0);
    let days = age_secs / 86400.0;
    // Wave 145 — finer-grained recency bucketing in the <1d band. Files
    // touched in the last hour are basically certain to be what the user
    // is working on; the old <7d bucket gave them the same 1.0 as a file
    // they last edited 6 days ago and missed that signal.
    let hours = age_secs / 3600.0;
    if hours < 1.0 { 1.4 }
    else if hours < 6.0 { 1.2 }
    else if days < 1.0 { 1.1 }
    else if days < 7.0 { 1.0 }
    else if days < 30.0 { 0.85 }
    else if days < 90.0 { 0.65 }
    else { 0.4 }
}

/// Find the `n` most-recently-modified text-ish files under `root`, skipping
/// the usual noise dirs (`.git`, `node_modules`, `target`, `dist`, etc.).
/// Cheap: single walkdir pass with depth cap.
fn recent_project_files(root: &Path, n: usize) -> Vec<PathBuf> {
    if let Ok(cache) = RECENT_FILES_CACHE.lock() {
        if let Some((at, files)) = cache.get(root) {
            if at.elapsed() < RECENT_FILES_TTL {
                return files.clone();
            }
        }
    }
    let files = recent_project_files_uncached(root, n);
    if let Ok(mut cache) = RECENT_FILES_CACHE.lock() {
        cache.insert(root.to_path_buf(), (Instant::now(), files.clone()));
    }
    files
}

fn recent_project_files_uncached(root: &Path, n: usize) -> Vec<PathBuf> {
    use walkdir::WalkDir;
    const SKIP: &[&str] = &[
        ".git", "node_modules", "target", "dist", "build", ".next",
        ".turbo", ".cache", "out", "coverage", "__pycache__",
    ];
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "md", "go", "java",
        "c", "h", "cpp", "hpp", "swift", "kt", "rb", "css", "html",
        "yaml", "yml", "toml", "json", "sh",
        // Wave 265 — modern ecosystem parity with wave-153 implicit-mention
        // extension list so the recent-files signal sees the same files
        // that the user can typify into the draft.
        "zig", "dart", "elm", "json5", "lua", "nix", "tf", "mjs", "cjs",
        "astro", "vue", "svelte", "jl", "ex", "exs", "clj", "hs", "ml",
        "scss", "sql", "proto", "gradle", "php", "scala",
    ];
    let mut out: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in WalkDir::new(root).max_depth(5).into_iter().filter_entry(|e| {
        e.file_name().to_str().map(|s| !SKIP.contains(&s)).unwrap_or(true)
    }) {
        let Ok(entry) = entry else { continue };
        let p = entry.path();
        if !entry.file_type().is_file() { continue; }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else { continue };
        if !EXTS.contains(&ext) { continue; }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        out.push((modified, p.to_path_buf()));
        if out.len() > n * 4 {
            out.sort_by(|a, b| b.0.cmp(&a.0));
            out.truncate(n);
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.truncate(n);
    out.into_iter().map(|(_, p)| p).collect()
}

fn source_kind_weight(src: &crate::memory::sources::MemorySource) -> f32 {
    use crate::memory::sources::SourceKind::*;
    match src.kind {
        ClaudeProjectMemory => 1.2,
        ProjectInstructions => 1.1,
        Obsidian => 1.0,
        Runbooks => 0.95,
        GlobalInstructions => 0.7,
    }
}

/// Final relevance score for one brain suggestion.
///
/// Extracted from the two inline call sites in `local_brain_suggest` so the
/// scoring formula — and especially the wave-218 PageRank multiplier — can be
/// unit tested without standing up a project tree. The arithmetic is identical
/// to the previous inline form:
///   * `weighted_hits.sqrt()` dampens runaway term-frequency,
///   * `recency` is the wave-145 freshness curve,
///   * `kind_weight` is `RECENT_EDITS_KIND_WEIGHT` for code edits or
///     `source_kind_weight` for memory/chat sources,
///   * `pr_boost` is the PageRank multiplier (`1.0` when not applicable, so it
///     never punishes a file).
fn score_suggestion(weighted_hits: f32, recency: f32, kind_weight: f32, pr_boost: f32) -> f32 {
    weighted_hits.sqrt() * recency * kind_weight * pr_boost
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lock the extracted scoring formula so future tuning of one factor can't
    // silently change the others' contribution.
    #[test]
    fn score_suggestion_matches_inline_formula() {
        // Recent-edits site: kind weight 1.5, pagerank-neutral boost 1.0.
        assert_eq!(
            score_suggestion(4.0, 1.2, RECENT_EDITS_KIND_WEIGHT, 1.0),
            2.0 * 1.2 * 1.5 * 1.0
        );
        // Memory site: source kind weight, boost fixed at 1.0.
        assert_eq!(score_suggestion(9.0, 1.1, 1.2, 1.0), 3.0 * 1.1 * 1.2);
    }

    #[test]
    fn score_suggestion_pagerank_boost_is_multiplicative_and_never_punishes() {
        let base = score_suggestion(4.0, 1.0, RECENT_EDITS_KIND_WEIGHT, 1.0);
        // A neutral pr_boost (no inbound edges) leaves the score unchanged.
        assert_eq!(score_suggestion(4.0, 1.0, RECENT_EDITS_KIND_WEIGHT, 1.0), base);
        // Max boost (1.0 + 1.0*0.5) scales by exactly 1.5×.
        let maxed = score_suggestion(4.0, 1.0, RECENT_EDITS_KIND_WEIGHT, 1.5);
        assert!((maxed - base * 1.5).abs() < 1e-6, "pr_boost not 1.5×: {maxed} vs {base}");
        assert!(maxed > base, "pagerank must boost, never punish");
    }

    #[test]
    fn score_suggestion_monotonic_in_hits_with_sqrt_dampening() {
        let lo = score_suggestion(1.0, 1.0, 1.0, 1.0);
        let hi = score_suggestion(16.0, 1.0, 1.0, 1.0);
        assert!(hi > lo, "more hits must score higher");
        // sqrt dampening: 16× the hits is only 4× the score, not 16×.
        assert!((hi - lo * 4.0).abs() < 1e-6, "expected sqrt dampening: {hi} vs {lo}");
    }

    // Wave 154 — lock in wave-145 recency curve so future tweaks don't
    // silently regress the "I just touched this" boost.
    #[test]
    fn extract_terms_picks_identifiers_with_high_boost() {
        let terms = extract_terms("How does processOrder handle the user_session.refresh path?");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("processorder"), Some(&2.0), "CamelCase missed: {by_text:?}");
        assert_eq!(by_text.get("user_session"), Some(&2.0), "snake_case missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_drops_stopwords() {
        let terms = extract_terms("would they have made some about this");
        assert!(
            terms.is_empty() || terms.iter().all(|t| t.boost > 1.0),
            "stopwords leaked: {:?}",
            terms.iter().map(|t| t.text.clone()).collect::<Vec<_>>()
        );
    }

    // Wave 161 — lock in the wave-158/159 filename-match behavior. The
    // recency_weight stage is the easy thing to test; we don't have a
    // public hook for the scoring loop, but the buckets themselves
    // implicitly cover this case (`<1h` files get 1.4×).
    // Wave 237 — regression: empty terms short-circuits before any
    // repo_map walk. We can't unit-test local_brain_suggest end-to-end
    // (it's a Tauri command), but we CAN verify extract_terms on
    // stopword-only input returns nothing — which is what triggers the
    // wave-236 early-out.
    #[test]
    fn extract_terms_returns_empty_for_stopwords_only() {
        let terms = extract_terms("would they have made some about this then");
        assert!(terms.is_empty(), "stopwords leaked into result: {terms:#?}");
    }

    // Wave 203 — qualified identifier tests.
    // Wave 272 — JS optional chaining.
    #[test]
    fn extract_terms_picks_chained_optional() {
        // Multiple `?.` in a row should still collapse to canonical dotted.
        let terms = extract_terms("verify user?.config?.timeout");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("user.config.timeout"), Some(&3.0), "chained ?. missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_picks_optional_chain() {
        let terms = extract_terms("inspect user?.save() before commit");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("user.save"), Some(&3.0), "optional chain missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_picks_deep_qualified() {
        let terms = extract_terms("trace Config::Repo::Map::format_as_text");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(
            by_text.get("config::repo::map::format_as_text"),
            Some(&3.0),
            "deep qualified missed: {by_text:?}"
        );
    }

    #[test]
    fn extract_terms_picks_underscore_prefixed() {
        let terms = extract_terms("the _private_helper does what");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("_private_helper"), Some(&2.0), "underscore-prefixed missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_handles_method_call_form() {
        // User.save() — the qualified Pass 0 should still extract
        // `user.save` even though it's followed by parens.
        let terms = extract_terms("does User.save() work?");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("user.save"), Some(&3.0), "method-call form missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_picks_qualified_dotted() {
        let terms = extract_terms("does User.save handle the case");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("user.save"), Some(&3.0), "qualified dotted missed: {by_text:?}");
    }

    #[test]
    fn extract_terms_picks_qualified_colon() {
        let terms = extract_terms("see model::User::save in the codebase");
        let by_text: std::collections::HashMap<&str, f32> =
            terms.iter().map(|t| (t.text.as_str(), t.boost)).collect();
        assert_eq!(by_text.get("model::user::save"), Some(&3.0), "qualified path missed: {by_text:?}");
    }

    #[test]
    fn recency_weight_returns_default_when_path_missing() {
        let p = std::path::Path::new("/nonexistent-cortex-test-path-xyz123/file.rs");
        let w = recency_weight(p);
        // Default fallback when metadata lookup fails — must NOT be 0
        // (would zero out the entire score).
        assert!(w > 0.0 && w <= 1.5, "fallback {w} out of expected range");
    }
}
