//! Semantic search over chat history — the cross-corpus twin of
//! `semantic_memory_search` (which covers the vault). Embeds chat messages
//! (incl. imported Claude.ai / ChatGPT history) via the existing Ollama embedder
//! ([`crate::memory::embed`]) and ranks by cosine similarity over a PERSISTED
//! vector index (`chat_embeddings` in cortex-local.db). Degrades gracefully:
//! with no Ollama or an empty index it returns nothing rather than hard-failing.
//!
//! This is the keystone "search your whole brain" gap: notes already had
//! semantic search; chats now do too, reusing the same embed model + cosine so
//! the two corpora are directly comparable for a future unified RAG path.

use crate::app_state::AppState;
use crate::memory::embed::{cosine, embed_model, embed_text};
use crate::memory::sources::{default_sources, walk_markdown};
use crate::observability::tracing_store::TracingStore;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::State;

/// Bytes of each message fed to the embedder (head). Kept comfortably under
/// mxbai-embed-large's ~512-token window — longer/denser inputs (code, logs)
/// make Ollama 500, so we stay conservative to maximize index coverage.
const EMBED_BYTES: usize = 1000;
/// Per-reindex cap so a huge history can't block one call indefinitely; the
/// index is incremental, so calling again continues where it left off.
const REINDEX_BATCH: i64 = 2000;

#[derive(Debug, Serialize)]
pub struct ReindexResult {
    pub embedded: usize,
    pub failed: usize,
    pub total_indexed: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSemanticHit {
    pub session_id: String,
    pub message_id: String,
    pub ts: i64,
    pub role: String,
    pub snippet: String,
    pub score: f32,
    /// Owning project root (chat: the message's project; note: the project
    /// dir the source file lives under), or `None` for global/unscoped
    /// content. Lets RAG retrieval scope results per-project (issue 010 full
    /// scope) without a second lookup.
    pub project_root: Option<String>,
}

/// Char-boundary-safe head of a string.
fn head(s: &str, max: usize) -> String {
    let t = s.trim();
    let mut end = max.min(t.len());
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    t[..end].to_string()
}

/// Embed every not-yet-embedded chat message for the active embed model.
/// Incremental + idempotent. Shared by the Tauri command and the mobile endpoint.
pub async fn reindex(
    store: &TracingStore,
    ollama_base: &str,
    model: &str,
) -> Result<ReindexResult, String> {
    if ollama_base.trim().is_empty() {
        return Err(
            "no Ollama base URL configured (set ollama_base_url in ~/.cortex/infra.json)".into(),
        );
    }
    let pending = store
        .messages_needing_embedding(model, REINDEX_BATCH)
        .map_err(|e| e.to_string())?;
    let mut embedded = 0usize;
    let mut failed = 0usize;
    for (id, session_id, ts, role, content, project_root) in pending {
        let text = head(&content, EMBED_BYTES);
        if text.is_empty() {
            continue;
        }
        match embed_text(ollama_base, model, &text).await {
            // Store the FULL content as the snippet source; embed only the head.
            Ok(v) if store
                .upsert_chat_embedding(&id, &session_id, ts, &role, &content, model, &v, project_root.as_deref())
                .is_ok() =>
            {
                embedded += 1;
            }
            _ => failed += 1,
        }
    }
    Ok(ReindexResult {
        embedded,
        failed,
        total_indexed: store.chat_embedding_count(model),
    })
}

/// Current mtime of `p` in unix ms; 0 when the file is missing or its
/// metadata is unreadable. Shared with `brain_rag`'s stale-chunk detection,
/// which compares this against the mtime recorded at embed time.
pub(crate) fn file_mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Embed all new/changed Obsidian + memory notes into the shared index
/// (role="note", message_id=path, ts=mtime), so the unified search/RAG covers
/// the WHOLE brain — notes AND chats — by meaning, not just the lexically-capped
/// hybrid. Incremental by mtime; the chat-export dir is skipped (already in the
/// chat index).
pub async fn reindex_notes(
    store: &TracingStore,
    ollama_base: &str,
    model: &str,
    vault: Option<PathBuf>,
    project_root: Option<PathBuf>,
) -> Result<ReindexResult, String> {
    if ollama_base.trim().is_empty() {
        return Err(
            "no Ollama base URL configured (set ollama_base_url in ~/.cortex/infra.json)".into(),
        );
    }
    let existing: HashMap<String, i64> = store.note_mtimes(model).into_iter().collect();

    let sources = default_sources(project_root.as_deref(), vault.as_deref());
    let mut seen: HashSet<PathBuf> = HashSet::new();
    // Paired with the owning source's `owner_project` (issue 010 full scope:
    // per-project memory namespaces) so each embedded chunk inherits the tag
    // that later scopes it to that project at retrieval time. `None` = global
    // (visible from every project).
    let mut paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for src in &sources {
        for p in walk_markdown(src) {
            // The chat-export dir is already covered by the chat index — skip it
            // so a Save-to-Brain note isn't double-embedded.
            if p.components().any(|c| c.as_os_str() == "Cortex Chats") {
                continue;
            }
            if seen.insert(p.clone()) {
                paths.push((p, src.owner_project.clone()));
            }
        }
    }

    // Filter to actually-needing-work BEFORE the batch cap (mirrors the chat
    // path's "WHERE not-embedded LIMIT"): otherwise an unchanged front of the
    // corpus consumes the whole batch and notes past REINDEX_BATCH never get
    // embedded. With the filter, successive passes drain any backlog.
    let needs_work: Vec<(PathBuf, String, i64, Option<PathBuf>)> = paths
        .into_iter()
        .filter_map(|(p, owner)| {
            let mtime = file_mtime_ms(&p);
            // mtime==0 means metadata was unreadable (e.g. a transient WSL UNC
            // failure) — skip rather than embed a ts=0 row that flip-flops.
            if mtime == 0 {
                return None;
            }
            let path_str = p.display().to_string();
            if existing.get(&path_str) == Some(&mtime) {
                return None; // unchanged since last embed
            }
            Some((p, path_str, mtime, owner))
        })
        .take(REINDEX_BATCH as usize)
        .collect();

    let mut embedded = 0usize;
    let mut failed = 0usize;
    for (p, path_str, mtime, owner) in needs_work {
        let content = match std::fs::read_to_string(&p) {
            Ok(c) => c,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let text = head(&content, EMBED_BYTES);
        if text.is_empty() {
            continue;
        }
        let owner_str = owner.as_ref().map(|p| p.display().to_string());
        match embed_text(ollama_base, model, &text).await {
            Ok(v) if store
                .upsert_chat_embedding(&path_str, "vault", mtime, "note", &content, model, &v, owner_str.as_deref())
                .is_ok() =>
            {
                embedded += 1;
            }
            _ => failed += 1,
        }
    }
    Ok(ReindexResult {
        embedded,
        failed,
        total_indexed: store.chat_embedding_count(model),
    })
}

/// Set while a `reindex_all` pass is in flight. Makes reindex single-flight so
/// the 30-min auto-index tick, the startup pass, and a manual reindex can never
/// stack and waste work embedding the same items concurrently.
static REINDEXING: AtomicBool = AtomicBool::new(false);

/// Refresh BOTH indexes (chat messages + vault/memory notes) incrementally.
/// Used by the manual reindex command/endpoint and the continuous auto-index.
/// Single-flight: returns an error immediately if a pass is already running.
pub async fn reindex_all(
    store: &TracingStore,
    ollama_base: &str,
    vault: Option<PathBuf>,
    project_root: Option<PathBuf>,
) -> Result<ReindexResult, String> {
    if REINDEXING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a reindex is already running".into());
    }
    // Clear the flag no matter how we leave (early `?`, panic, normal return).
    struct ResetOnDrop;
    impl Drop for ResetOnDrop {
        fn drop(&mut self) {
            REINDEXING.store(false, Ordering::SeqCst);
        }
    }
    let _reset = ResetOnDrop;

    let model = embed_model();
    let chats = reindex(store, ollama_base, &model).await?;
    // Notes are best-effort — a notes failure shouldn't sink a successful chat
    // pass.
    let notes = reindex_notes(store, ollama_base, &model, vault, project_root)
        .await
        .unwrap_or(ReindexResult { embedded: 0, failed: 0, total_indexed: 0 });
    Ok(ReindexResult {
        embedded: chats.embedded + notes.embedded,
        failed: chats.failed + notes.failed,
        total_indexed: store.chat_embedding_count(&model),
    })
}

/// Semantic search over the shared index: embed the query, cosine-rank stored
/// vectors, return the top `limit`. `include_notes` controls whether vault/memory
/// notes (role="note") participate — RAG wants the whole brain (true), the
/// chat-history search surfaces want chats only (false) so results stay clickable
/// as sessions.
pub async fn search(
    store: &TracingStore,
    ollama_base: &str,
    model: &str,
    query: &str,
    limit: usize,
    include_notes: bool,
) -> Result<Vec<ChatSemanticHit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("empty query".into());
    }
    let rows = store.all_chat_embeddings(model).map_err(|e| e.to_string())?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let q = embed_text(ollama_base, model, query)
        .await
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<ChatSemanticHit> = rows
        .into_iter()
        .filter(|(_, _, _, role, _, _, _)| include_notes || role != "note")
        .map(|(message_id, session_id, ts, role, text, vec, project_root)| ChatSemanticHit {
            score: cosine(&q, &vec),
            snippet: head(&text, 280),
            session_id,
            message_id,
            ts,
            role,
            project_root,
        })
        .collect();
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit.clamp(1, 50));
    Ok(hits)
}

// ── Tauri commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn semantic_chat_reindex(
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ReindexResult, String> {
    let (base, vault) = {
        let c = state.config.read();
        (c.ollama_base_url.clone(), c.obsidian_vault.clone())
    };
    reindex_all(store.inner(), &base, vault, None).await
}

#[tauri::command]
pub async fn semantic_chat_search(
    query: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<Vec<ChatSemanticHit>, String> {
    let base = state.config.read().ollama_base_url.clone();
    search(store.inner(), &base, &embed_model(), &query, limit.unwrap_or(10), false).await
}
