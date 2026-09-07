//! `find_duplicate_memory` — cheap Jaccard-based duplicate detection across
//! every markdown file the memory subsystem already knows about.
//!
//! We deliberately keep the algorithm small (set-based, no embeddings) so it
//! runs on the UI thread tier of latency. Files are tokenised once into
//! lowercase alpha tokens >= 4 chars with English stopwords removed, then
//! pairwise compared. Anything above `threshold` (default 0.4) is returned
//! sorted by similarity desc, capped at 50 pairs.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::app_state::AppState;
use crate::memory::sources;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct DuplicatePair {
    pub file_a: String,
    pub file_b: String,
    pub similarity: f32,
    pub shared_words: Vec<String>,
}

const MAX_PAIRS: usize = 50;
const DEFAULT_THRESHOLD: f32 = 0.4;
const TOP_SHARED_WORDS: usize = 10;
/// Hard cap on how many documents enter the O(n^2) pairwise stage. This runs
/// on the UI thread tier of latency, so an unbounded document count (e.g. a
/// large vault) could stall the UI for seconds. 1500 docs => ~1.1M pairs,
/// which stays comfortably sub-second; beyond that we stop collecting.
const MAX_DOCS: usize = 1500;
/// Files larger than this are skipped — we already cap markdown walking at
/// 1 MiB, but a per-file safety net keeps the worst case bounded.
const MAX_FILE_BYTES: u64 = 512 * 1024;

#[tauri::command]
pub async fn find_duplicate_memory(
    threshold: Option<f32>,
    active_project: Option<String>,
    obsidian_vault: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DuplicatePair>, String> {
    let thresh = threshold.unwrap_or(DEFAULT_THRESHOLD).clamp(0.0, 1.0);

    let active = active_project.as_ref().map(PathBuf::from);
    let vault = obsidian_vault
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| state.config.read().obsidian_vault.clone());
    let srcs = sources::default_sources(active.as_deref(), vault.as_deref());

    // Tokenise each file once, drop empty ones up-front.
    let mut docs: Vec<(PathBuf, HashSet<String>)> = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    for src in &srcs {
        for path in sources::walk_markdown(src) {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else { continue };
            let tokens = tokenize(&body);
            if tokens.len() < 5 {
                // Tiny files generate noisy false positives — skip them.
                continue;
            }
            docs.push((path, tokens));
            if docs.len() >= MAX_DOCS {
                // Bound the O(n^2) pairwise stage so a large vault can't stall
                // the UI thread. We stop collecting rather than letting the
                // document count grow without limit.
                break;
            }
        }
        if docs.len() >= MAX_DOCS {
            break;
        }
    }

    // Pairwise Jaccard. O(n^2) over file count; with the 1 MiB / 512 KiB
    // caps this comfortably handles thousands of markdown files in well
    // under a second on the typical homelab.
    let mut pairs: Vec<DuplicatePair> = Vec::new();
    for i in 0..docs.len() {
        for j in (i + 1)..docs.len() {
            let (sim, shared) = jaccard_with_shared(&docs[i].1, &docs[j].1, TOP_SHARED_WORDS);
            if sim >= thresh {
                pairs.push(DuplicatePair {
                    file_a: docs[i].0.display().to_string(),
                    file_b: docs[j].0.display().to_string(),
                    similarity: sim,
                    shared_words: shared,
                });
            }
        }
    }

    pairs.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pairs.truncate(MAX_PAIRS);
    Ok(pairs)
}

/// Lowercase, alpha-only, length >= 4, stopwords removed. Returns the unique
/// token set — Jaccard works on sets, so duplicates within a single document
/// don't matter.
fn tokenize(text: &str) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let mut buf = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            buf.push(ch.to_ascii_lowercase());
        } else {
            if buf.len() >= 4 && !is_stopword(&buf) {
                out.insert(std::mem::take(&mut buf));
            } else {
                buf.clear();
            }
        }
    }
    if buf.len() >= 4 && !is_stopword(&buf) {
        out.insert(buf);
    }
    out
}

/// Compact English stopword list. Words shorter than 4 chars are already
/// filtered out by the tokenizer, so we only need to drop common 4+ char
/// noise here.
fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "this"
            | "that"
            | "with"
            | "have"
            | "from"
            | "they"
            | "them"
            | "then"
            | "than"
            | "when"
            | "what"
            | "which"
            | "your"
            | "yours"
            | "their"
            | "there"
            | "here"
            | "been"
            | "were"
            | "where"
            | "into"
            | "about"
            | "only"
            | "also"
            | "some"
            | "such"
            | "those"
            | "these"
            | "would"
            | "could"
            | "should"
            | "after"
            | "before"
            | "over"
            | "under"
            | "more"
            | "most"
            | "other"
            | "just"
            | "like"
            | "make"
            | "made"
            | "many"
            | "much"
            | "very"
            | "even"
            | "still"
            | "while"
            | "each"
            | "both"
            | "above"
            | "between"
    )
}

fn jaccard_with_shared(
    a: &HashSet<String>,
    b: &HashSet<String>,
    top: usize,
) -> (f32, Vec<String>) {
    if a.is_empty() || b.is_empty() {
        return (0.0, Vec::new());
    }
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut shared: Vec<&String> = small.iter().filter(|t| large.contains(*t)).collect();
    let inter = shared.len();
    if inter == 0 {
        return (0.0, Vec::new());
    }
    let union = a.len() + b.len() - inter;
    let sim = inter as f32 / union as f32;
    // Pick the longest shared words first — they tend to be the most
    // distinctive ("kubernetes" > "files"), giving the UI a useful preview.
    shared.sort_by(|x, y| y.len().cmp(&x.len()));
    let top: Vec<String> = shared.into_iter().take(top).cloned().collect();
    (sim, top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_strips_short_and_symbols() {
        let toks = tokenize("Hello, world! A bigger word goes here.");
        assert!(toks.contains("hello"));
        assert!(toks.contains("world"));
        assert!(toks.contains("bigger"));
        // "A" too short, "is" too short, "the" too short
        assert!(!toks.contains("a"));
    }

    #[test]
    fn tokenize_drops_stopwords() {
        let toks = tokenize("this that with from word");
        assert!(!toks.contains("this"));
        assert!(!toks.contains("that"));
        assert!(toks.contains("word"));
    }

    #[test]
    fn jaccard_identical() {
        let a = tokenize("alpha beta gamma delta epsilon");
        let b = a.clone();
        let (sim, _) = jaccard_with_shared(&a, &b, 5);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint() {
        let a = tokenize("alpha beta");
        let b = tokenize("gamma delta epsilon");
        let (sim, shared) = jaccard_with_shared(&a, &b, 5);
        assert!(sim < 0.1);
        assert!(shared.is_empty());
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a = tokenize("kubernetes deployment cluster nodes");
        let b = tokenize("kubernetes deployment service nodes");
        let (sim, shared) = jaccard_with_shared(&a, &b, 5);
        assert!(sim > 0.4 && sim < 0.9);
        assert!(shared.iter().any(|w| w == "kubernetes"));
    }
}
