//! Semantic memory search — builds on the existing vault sources
//! (`memory::sources`) + the existing Ollama config. Retrieves vault markdown
//! lexically (bounded candidate set), then re-ranks by embedding cosine
//! similarity via Ollama. Degrades gracefully to lexical order when Ollama or
//! the embedding model is unavailable, so it never hard-fails.

use crate::app_state::AppState;
use crate::memory::embed::{cosine, embed_model, embed_text};
use crate::memory::sources::{default_sources, walk_markdown};
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

/// Cap on candidate docs we embed per query — keeps it to ~K+1 Ollama calls.
const MAX_CANDIDATES: usize = 24;
/// Bytes of each doc fed to the embedder (head of the file). Kept well under
/// mxbai-embed-large's ~512-token window — longer inputs make Ollama 500.
const SNIPPET_BYTES: usize = 1200;

#[derive(Debug, Serialize)]
pub struct SemanticHit {
    pub path: String,
    pub snippet: String,
    pub score: f32,
    /// "semantic" when re-ranked by embeddings, "lexical" on fallback.
    pub mode: String,
}

/// Head of a document, char-boundary-safe, capped at SNIPPET_BYTES.
fn head_snippet(content: &str) -> String {
    let trimmed = content.trim();
    let mut end = SNIPPET_BYTES.min(trimmed.len());
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}

fn snippet_of(path: &PathBuf) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let snip = head_snippet(&content);
    if snip.is_empty() {
        None
    } else {
        Some(snip)
    }
}

/// Lexical pre-filter: score by count of query-word occurrences (case-insensitive)
/// so we only embed the most promising candidates.
fn lexical_score(text: &str, terms: &[String]) -> usize {
    let lc = text.to_lowercase();
    terms.iter().map(|t| lc.matches(t.as_str()).count()).sum()
}

#[tauri::command]
pub async fn semantic_memory_search(
    query: String,
    project_root: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<SemanticHit>, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("empty query".into());
    }
    let limit = limit.unwrap_or(10).clamp(1, 50);
    let (ollama_base, vault) = {
        let cfg = state.config.read();
        (cfg.ollama_base_url.clone(), cfg.obsidian_vault.clone())
    };
    let active = project_root.map(PathBuf::from);

    // scored: (display-path, snippet, lexical-score)
    let mut scored: Vec<(String, String, usize)> = Vec::new();

    // 0. Prefer Obsidian's live, indexed search when the Local REST API plugin
    //    is running — strictly better candidates than a filesystem walk. Falls
    //    through to the walk if the plugin is unavailable or returns nothing.
    if let Some(vault_path) = vault.as_deref() {
        if let Some(client) = crate::memory::obsidian_rest::RestClient::from_vault(vault_path) {
            if client.status().await {
                if let Ok(hits) = client.search(&query, MAX_CANDIDATES).await {
                    for h in hits {
                        if let Ok(body) = client.read_note(&h.path).await {
                            let snip = head_snippet(&body);
                            if !snip.is_empty() {
                                scored.push((vault_path.join(&h.path).display().to_string(), snip, 1));
                            }
                        }
                    }
                    if !scored.is_empty() {
                        tracing::info!(target: "cortex::semantic", n = scored.len(), "candidates via Obsidian REST");
                    }
                }
            }
        }
    }

    // 1. Fallback: gather + lexically pre-filter vault/project markdown.
    if scored.is_empty() {
        let candidates: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
            let sources = default_sources(active.as_deref(), vault.as_deref());
            let mut files = Vec::new();
            for src in &sources {
                files.extend(walk_markdown(src));
            }
            files
        })
        .await
        .map_err(|e| e.to_string())?;

        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|w| w.len() > 1)
            .map(|w| w.to_string())
            .collect();
        for path in candidates {
            if let Some(snip) = snippet_of(&path) {
                let s = if terms.is_empty() { 1 } else { lexical_score(&snip, &terms) };
                if s > 0 || terms.is_empty() {
                    scored.push((path.display().to_string(), snip, s));
                }
            }
        }
        scored.sort_by(|a, b| b.2.cmp(&a.2));
        scored.truncate(MAX_CANDIDATES);
    }

    if scored.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Try to embed the query + candidates and re-rank by cosine. Any failure
    //    falls back to the lexical order already computed.
    let model = embed_model();
    tracing::info!(target: "cortex::semantic", url = %ollama_base, model = %model, candidates = scored.len(), "semantic_memory_search embedding");
    let mut hits: Vec<SemanticHit> = match embed_text(&ollama_base, &model, &query).await {
        Ok(q_vec) => {
            // Embed each candidate; SKIP (don't abort) any that fail so one bad
            // snippet can't sink the whole semantic ranking. Only if NONE embed
            // do we fall back to lexical (handled below via empty `out`).
            // Embed candidates with bounded concurrency (Ollama serializes on
            // the model, but pipelining hides per-request HTTP overhead and cuts
            // wall-clock vs strictly sequential). Skip-on-error preserved.
            use futures::stream::StreamExt;
            use std::sync::Arc;
            let q = Arc::new(q_vec);
            // Own all captured data (Arc the 1KB query vec) so no borrow crosses
            // the concurrent futures — that trips the tauri command lifetime bound.
            let mut out: Vec<SemanticHit> = futures::stream::iter(scored.iter().cloned())
                .map(|(path, snip, _lex)| {
                    let q = Arc::clone(&q);
                    let base = ollama_base.clone();
                    let m = model.clone();
                    async move {
                        match embed_text(&base, &m, &snip).await {
                            Ok(v) => Some(SemanticHit {
                                path,
                                snippet: snip.chars().take(280).collect(),
                                score: cosine(&q, &v),
                                mode: "semantic".into(),
                            }),
                            Err(_) => None,
                        }
                    }
                })
                .buffer_unordered(6)
                .filter_map(|x| async move { x })
                .collect()
                .await;
            let failures = scored.len().saturating_sub(out.len());
            if failures > 0 {
                tracing::warn!(target: "cortex::semantic", failures, embedded = out.len(), "some candidate embeds failed (skipped)");
            }
            out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            out
        }
        Err(e) => {
            tracing::warn!(target: "cortex::semantic", error = %e, "query embed failed → lexical fallback");
            Vec::new()
        }
    };

    // Fallback: lexical order (scored is already sorted by lexical score).
    if hits.is_empty() {
        hits = scored
            .into_iter()
            .map(|(path, snip, lex)| SemanticHit {
                path,
                snippet: snip.chars().take(280).collect(),
                score: lex as f32,
                mode: "lexical".into(),
            })
            .collect();
    }

    hits.truncate(limit);
    Ok(hits)
}
