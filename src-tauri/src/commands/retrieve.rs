//! `retrieve` Tauri command — thin wrapper over the unified context-retrieval
//! pipeline. Validates inputs (non-empty query, existing project directory)
//! and delegates to [`crate::retrieval::retrieve_blended`] on a blocking pool
//! thread (the pipeline does synchronous filesystem + sqlite work).

use crate::retrieval::{retrieve_blended, RetrievalHit};

/// Re-export so the frontend bindings / callers can refer to the hit type from
/// the command module as well as from `retrieval`.
#[allow(unused_imports)]
pub use crate::retrieval::RetrievalHit as RetrieveHit;

/// Gather, dedup, rerank and return the top-`k` context hits for `query`
/// rooted at `project_root`.
///
/// - `query` must be non-empty (after trimming).
/// - `project_root` must exist and be a directory.
/// - `k` defaults to 10 when omitted.
#[tauri::command]
pub async fn retrieve(
    project_root: String,
    query: String,
    k: Option<usize>,
) -> Result<Vec<RetrievalHit>, String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".into());
    }
    let root = std::path::PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let k = k.unwrap_or(10);

    tokio::task::spawn_blocking(move || retrieve_blended(&root, &query, k))
        .await
        .map_err(|e| format!("join error: {e}"))
}
