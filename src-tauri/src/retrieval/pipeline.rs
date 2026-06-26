//! Unified context-retrieval pipeline with a lightweight, explainable reranker.
//!
//! Gathers candidate context from the three retrieval sources that already
//! exist in this codebase, dedups by path, and reranks by a blended score:
//!   - symbols  (`repo_map::repo_symbols`)              weight 1.0
//!   - memory   (`memory::chroma::substring_search`)    weight 0.8
//!   - recent   (recently-edited project files)         weight 0.5
//!
//! The reranker is intentionally simple so it is explainable: each source's
//! raw scores are min-max normalized to 0..1, multiplied by the per-source
//! weight, then boosted when the hit's path/snippet contains query tokens.
//! Dedup keeps the max score per path. No network calls, no async — this is a
//! pure function safe to call from a `spawn_blocking` closure.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::memory::chroma::substring_search;
use crate::repo_map::repo_symbols;

/// Per-source blend weights. Symbols are the strongest structural signal,
/// memory is supporting context, recent edits are a weak recency prior.
const W_SYMBOL: f32 = 1.0;
const W_MEMORY: f32 = 0.8;
const W_RECENT: f32 = 0.5;

/// How many candidates to pull from each source before reranking.
const PER_SOURCE_LIMIT: usize = 20;
/// Recent-files walk cap.
const RECENT_LIMIT: usize = 20;
/// Multiplicative boost applied per distinct query token found in a hit's
/// path or snippet. Keeps central / on-topic hits floating above generic ones.
const TOKEN_BOOST: f32 = 0.5;
/// Minimum token length considered for the boost (skip noise like "of"/"to").
const MIN_TOKEN_LEN: usize = 3;

/// A single reranked context hit returned by [`retrieve_blended`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalHit {
    /// One of: "symbol" | "memory" | "recent".
    pub source: String,
    pub path: String,
    pub snippet: String,
    pub score: f32,
}

/// Internal pre-rank candidate carrying its raw (un-normalized) source score.
struct Candidate {
    source: &'static str,
    path: String,
    snippet: String,
    raw: f32,
}

/// Retrieve blended context for `query` rooted at `project_root`, returning the
/// top `k` reranked hits. Never panics; returns an empty vec when nothing
/// matches. The memory source is best-effort: if `substring_search` errors it
/// is silently skipped and the other sources still contribute.
pub fn retrieve_blended(project_root: &Path, query: &str, k: usize) -> Vec<RetrievalHit> {
    let k = if k == 0 { 10 } else { k };
    let tokens = query_tokens(query);

    let mut candidates: Vec<Candidate> = Vec::new();
    collect_symbols(project_root, query, &mut candidates);
    collect_memory(query, &mut candidates);
    collect_recent(project_root, query, &mut candidates);

    if candidates.is_empty() {
        return Vec::new();
    }

    // Normalize raw scores within each source independently so a source with
    // naturally large raw values doesn't dominate purely by magnitude.
    let normed = normalize_per_source(&candidates);

    // Blend: normalized * source-weight, then per-token containment boost.
    // Dedup by path keeping the max blended score.
    let mut best: HashMap<String, RetrievalHit> = HashMap::new();
    for (cand, norm) in candidates.iter().zip(normed.into_iter()) {
        let weight = match cand.source {
            "symbol" => W_SYMBOL,
            "memory" => W_MEMORY,
            "recent" => W_RECENT,
            _ => 0.0,
        };
        let mut score = norm * weight;
        score *= token_boost(&tokens, &cand.path, &cand.snippet);

        let entry = best
            .entry(cand.path.clone())
            .or_insert_with(|| RetrievalHit {
                source: cand.source.to_string(),
                path: cand.path.clone(),
                snippet: cand.snippet.clone(),
                score: f32::MIN,
            });
        if score > entry.score {
            entry.score = score;
            entry.source = cand.source.to_string();
            entry.snippet = cand.snippet.clone();
        }
    }

    let mut out: Vec<RetrievalHit> = best.into_values().collect();
    // Sort by score desc; tie-break on path for deterministic ordering.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    out.truncate(k);
    out
}

/// Lowercased, deduped query tokens of length >= [`MIN_TOKEN_LEN`].
fn query_tokens(query: &str) -> Vec<String> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for tok in query.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let lower = tok.to_lowercase();
        if lower.len() < MIN_TOKEN_LEN {
            continue;
        }
        if seen.insert(lower.clone(), ()).is_none() {
            out.push(lower);
        }
    }
    out
}

/// Word-boundary boost: 1.0 + TOKEN_BOOST for each distinct token found as a
/// whole word in the path or snippet (case-insensitive). A hit matching every
/// token floats well above one matching none. Matching on word boundaries (and
/// `_`) avoids false positives where a token is merely a substring of a larger
/// word (e.g. "order" inside "reorder"). The haystack is lowercased once and
/// scanned for all tokens, rather than rebuilt per token.
fn token_boost(tokens: &[String], path: &str, snippet: &str) -> f32 {
    if tokens.is_empty() {
        return 1.0;
    }
    let hay = format!("{}\n{}", path, snippet).to_lowercase();
    let mut boost = 1.0;
    for t in tokens {
        if contains_word(&hay, t) {
            boost += TOKEN_BOOST;
        }
    }
    boost
}

/// True when `needle` occurs in `hay` as a whole "word" — i.e. each occurrence
/// is bounded on both sides by a non-alphanumeric, non-`_` character (or the
/// string edge). Both inputs are assumed already lowercased. This mirrors the
/// tokenization in [`query_tokens`], which splits on the same boundary, so a
/// query token only boosts a hit that genuinely contains that token.
fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let at = start + pos;
        let before_ok = at == 0
            || !hay[..at]
                .chars()
                .next_back()
                .is_some_and(is_word);
        let after = at + needle.len();
        let after_ok = after >= bytes.len()
            || !hay[after..].chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

/// Min-max normalize raw scores within each source to [`NORM_FLOOR`]..1. When a
/// source has a single value (or all-equal values) every entry maps to 1.0 so
/// the source still contributes its full weight. The minimum is mapped to a
/// small positive floor rather than 0.0 so the lowest-ranked candidate retains
/// some of its source weight and can still be lifted by token boosts — mapping
/// it to exactly 0 would zero out its blended score regardless of how many
/// query tokens it matched, sinking strong textual matches to the bottom.
fn normalize_per_source(cands: &[Candidate]) -> Vec<f32> {
    /// Floor for the normalized score so the minimum raw doesn't collapse to 0.
    const NORM_FLOOR: f32 = 0.1;
    let mut min_max: HashMap<&str, (f32, f32)> = HashMap::new();
    for c in cands {
        let e = min_max.entry(c.source).or_insert((f32::MAX, f32::MIN));
        if c.raw < e.0 {
            e.0 = c.raw;
        }
        if c.raw > e.1 {
            e.1 = c.raw;
        }
    }
    cands
        .iter()
        .map(|c| {
            let (lo, hi) = min_max[c.source];
            let span = hi - lo;
            if span <= f32::EPSILON {
                1.0
            } else {
                NORM_FLOOR + (1.0 - NORM_FLOOR) * (c.raw - lo) / span
            }
        })
        .collect()
}

/// Symbol source. Rank within source by recency of appearance — earlier hits
/// (more recently-modified files) get a higher raw score.
fn collect_symbols(root: &Path, query: &str, out: &mut Vec<Candidate>) {
    let hits = repo_symbols(root, query, PER_SOURCE_LIMIT);
    let n = hits.len();
    for (i, h) in hits.into_iter().enumerate() {
        let raw = (n - i) as f32;
        let snippet = format!("{} {} (line {})", h.kind, h.name, h.line);
        out.push(Candidate {
            source: "symbol",
            path: h.path,
            snippet,
            raw,
        });
    }
}

/// Memory source. Best-effort — errors are swallowed so a missing/locked
/// chroma DB never fails the whole retrieval.
fn collect_memory(query: &str, out: &mut Vec<Candidate>) {
    let needle = query.trim();
    if needle.is_empty() {
        return;
    }
    let Ok(hits) = substring_search(needle, PER_SOURCE_LIMIT) else {
        return;
    };
    // `substring_search` emits a synthetic "schema-info" row when it falls
    // back to listing tables (no real document match). That is a diagnostic,
    // not context — drop it so it never pollutes retrieval results.
    let hits: Vec<_> = hits.into_iter().filter(|h| h.id != "schema-info").collect();
    let n = hits.len();
    for (i, h) in hits.into_iter().enumerate() {
        let raw = (n - i) as f32;
        // The chroma doc id is the most stable "path"-like key available.
        let path = if h.id.is_empty() {
            format!("memory:{i}")
        } else {
            format!("memory:{}", h.id)
        };
        let snippet: String = h.document.chars().take(200).collect();
        out.push(Candidate {
            source: "memory",
            path,
            snippet,
            raw,
        });
    }
}

/// Recent source. Walks the project's most-recently-modified files and scores
/// each by how many query tokens its name/content contains. Self-contained
/// (does not touch local_brain) to avoid coupling to a private module.
fn collect_recent(root: &Path, query: &str, out: &mut Vec<Candidate>) {
    use walkdir::WalkDir;
    const SKIP: &[&str] = &[
        ".git", "node_modules", "target", "dist", "build", ".next", ".turbo",
        ".cache", "out", "coverage", "__pycache__", ".venv", "venv",
    ];
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "md", "go", "java", "c", "h",
        "cpp", "hpp", "swift", "rb", "php", "cs", "toml", "json", "yaml", "yml",
    ];
    let tokens = query_tokens(query);

    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|s| !SKIP.contains(&s))
                .unwrap_or(true)
        })
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !EXTS.contains(&ext) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        files.push((modified, p.to_path_buf()));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(RECENT_LIMIT);

    let n = files.len();
    for (i, (_, p)) in files.into_iter().enumerate() {
        // Base raw is recency rank; add token hits in the basename so an
        // on-topic recent file outranks a merely-recent one.
        let mut raw = (n - i) as f32;
        let fname = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase();
        for t in &tokens {
            if fname.contains(t.as_str()) {
                raw += n as f32;
            }
        }
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(Candidate {
            source: "recent",
            path: rel,
            snippet: String::new(),
            raw,
        });
    }
}

/// Build the prompt body handed to the gateway for LLM reranking. Candidates
/// are presented index-numbered so the model can answer with a cheap permutation
/// of indices rather than echoing paths. Pure + deterministic so it is unit
/// testable without a network call.
pub fn build_rerank_prompt(query: &str, hits: &[RetrievalHit]) -> String {
    let mut s = String::new();
    s.push_str("Query: \"");
    s.push_str(query.trim());
    s.push_str("\"\n\nCandidates (index. [source] path — snippet):\n");
    for (i, h) in hits.iter().enumerate() {
        let snippet = h.snippet.replace('\n', " ");
        let snippet: String = snippet.chars().take(160).collect();
        if snippet.is_empty() {
            s.push_str(&format!("{i}. [{}] {}\n", h.source, h.path));
        } else {
            s.push_str(&format!("{i}. [{}] {} — {}\n", h.source, h.path, snippet));
        }
    }
    s.push_str(
        "\nReorder these candidates by relevance to the query, most relevant first. \
         Return ONLY the indices, comma-separated (e.g. \"3,0,2,1\"). \
         Include every index exactly once. No prose, no code fences.",
    );
    s
}

/// Parse the model's reranking answer into a full permutation of `0..n`.
///
/// Tolerant by design: pulls every integer token out of the response, keeps the
/// in-range ones (deduped, first-occurrence wins), then appends any indices the
/// model dropped in their original order. The result is ALWAYS a complete
/// permutation of `0..n`, so a malformed/empty/partial answer degrades to the
/// original heuristic order rather than losing candidates.
pub fn parse_rank_order(response: &str, n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut cur = String::new();
    let flush = |cur: &mut String, order: &mut Vec<usize>, seen: &mut [bool]| {
        if cur.is_empty() {
            return;
        }
        if let Ok(idx) = cur.parse::<usize>() {
            if idx < seen.len() && !seen[idx] {
                seen[idx] = true;
                order.push(idx);
            }
        }
        cur.clear();
    };
    for ch in response.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut order, &mut seen);
        }
    }
    flush(&mut cur, &mut order, &mut seen);
    // Append any indices the model omitted, preserving original order.
    for (i, hit) in seen.iter().enumerate() {
        if !hit {
            order.push(i);
        }
    }
    order
}

/// Reorder `hits` by `order` (a permutation of `0..hits.len()`), keeping the
/// first `k` entries. Indices out of range are skipped defensively.
pub fn apply_rank_order(hits: Vec<RetrievalHit>, order: &[usize], k: usize) -> Vec<RetrievalHit> {
    let k = if k == 0 { hits.len() } else { k };
    let mut by_index: Vec<Option<RetrievalHit>> = hits.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(k.min(by_index.len()));
    for &idx in order {
        if out.len() >= k {
            break;
        }
        if let Some(slot) = by_index.get_mut(idx) {
            if let Some(hit) = slot.take() {
                out.push(hit);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str) -> RetrievalHit {
        RetrievalHit {
            source: "symbol".into(),
            path: path.into(),
            snippet: String::new(),
            score: 1.0,
        }
    }

    #[test]
    fn parse_rank_order_full_permutation() {
        assert_eq!(parse_rank_order("3,0,2,1", 4), vec![3, 0, 2, 1]);
    }

    #[test]
    fn parse_rank_order_tolerates_prose_and_fills_missing() {
        // Model returned prose + only a partial list; missing indices (2,3) are
        // appended in original order, dropping out-of-range 9.
        let order = parse_rank_order("The order is: 1, then 0 (9 is irrelevant)", 4);
        assert_eq!(order, vec![1, 0, 2, 3]);
    }

    #[test]
    fn parse_rank_order_empty_is_identity() {
        assert_eq!(parse_rank_order("no numbers here", 3), vec![0, 1, 2]);
        assert_eq!(parse_rank_order("", 3), vec![0, 1, 2]);
    }

    #[test]
    fn parse_rank_order_dedups_repeats() {
        assert_eq!(parse_rank_order("0,0,1,1,0", 3), vec![0, 1, 2]);
    }

    #[test]
    fn apply_rank_order_reorders_and_truncates() {
        let hits = vec![hit("a.rs"), hit("b.rs"), hit("c.rs")];
        let out = apply_rank_order(hits, &[2, 0, 1], 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "c.rs");
        assert_eq!(out[1].path, "a.rs");
    }

    #[test]
    fn build_rerank_prompt_numbers_candidates() {
        let hits = vec![hit("alpha.rs"), hit("beta.rs")];
        let p = build_rerank_prompt("find alpha", &hits);
        assert!(p.contains("0. [symbol] alpha.rs"));
        assert!(p.contains("1. [symbol] beta.rs"));
        assert!(p.contains("find alpha"));
    }

    #[test]
    fn empty_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        // Use a query string that cannot match any real symbol, recent file,
        // or chroma memory document on the host so the result is deterministic
        // regardless of the developer's local memory store.
        let hits = retrieve_blended(dir.path(), "zzqq_nomatch_token_9f3a17c4", 5);
        assert!(hits.is_empty(), "expected empty, got {hits:?}");
    }

    #[test]
    fn finds_symbol_and_dedups_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A file whose path AND symbol match the query — should appear once.
        std::fs::write(
            root.join("processor.rs"),
            "pub fn processOrder() {}\npub struct OrderState {}\n",
        )
        .unwrap();
        let hits = retrieve_blended(root, "processOrder", 10);
        assert!(!hits.is_empty(), "expected at least one hit");
        // Dedup: processor.rs must not appear twice even though two symbols
        // matched the same file.
        let count = hits.iter().filter(|h| h.path == "processor.rs").count();
        assert_eq!(count, 1, "path not deduped: {hits:?}");
        assert_eq!(hits[0].source, "symbol");
    }

    #[test]
    fn ordering_prefers_token_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("alpha.rs"), "pub fn processOrder() {}\n").unwrap();
        std::fs::write(root.join("beta.rs"), "pub fn unrelatedThing() {}\n").unwrap();
        let hits = retrieve_blended(root, "processOrder", 10);
        // The file matching the query token should rank first.
        assert_eq!(hits[0].path, "alpha.rs", "token match not on top: {hits:?}");
        // Scores must be sorted descending.
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score, "out of order: {hits:?}");
        }
    }

    #[test]
    fn truncates_to_k() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..12 {
            std::fs::write(
                root.join(format!("f{i}.rs")),
                format!("pub fn func{i}() {{}}\npub struct Type{i} {{}}\n"),
            )
            .unwrap();
        }
        let hits = retrieve_blended(root, "", 3);
        assert!(hits.len() <= 3, "k-truncation failed: {} hits", hits.len());
    }

    #[test]
    fn query_tokens_filters_short_and_dedups() {
        let toks = query_tokens("Process the ProcessOrder of an Order");
        assert!(toks.contains(&"process".to_string()));
        assert!(toks.contains(&"processorder".to_string()));
        // "of"/"an" are below MIN_TOKEN_LEN.
        assert!(!toks.contains(&"of".to_string()));
        // "the" appears once only.
        assert_eq!(toks.iter().filter(|t| *t == "the").count(), 1);
    }
}
