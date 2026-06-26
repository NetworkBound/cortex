//! Memory-bridge health/stats panel backend.
//!
//! Walks every `crate::memory::sources::default_sources(None, None)` entry and
//! reports file counts, total bytes, and oldest/newest mtimes. Also surfaces
//! the state of the claude-mem chroma DB at `~/.claude-mem/chroma/chroma.sqlite3`
//! so the UI can show whether the upstream semantic index is present.
//!
//! Read-only: no writes, no mutations. `sync_memory` simply reuses the existing
//! `import_claude_mem` command so we don't duplicate that logic here.

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::commands::memory_bridge::{import_claude_mem, ImportSummary};
use crate::memory::sources::{default_sources, walk_markdown, MemorySource, SourceKind};

#[derive(Debug, Clone, Serialize)]
pub struct SourceStats {
    pub label: String,
    pub kind: SourceKind,
    pub root_path: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub oldest_unix_ms: Option<i64>,
    pub newest_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ChromaState {
    pub exists: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryStats {
    pub sources: Vec<SourceStats>,
    pub chroma: ChromaState,
    pub total_file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn memory_stats() -> Result<MemoryStats, String> {
    tokio::task::spawn_blocking(collect_stats)
        .await
        .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn sync_memory() -> Result<SyncReport, String> {
    // Reuse the existing import command so we don't drift from its dedupe
    // behaviour. `import_claude_mem` is async and returns its own Result.
    match import_claude_mem().await {
        Ok(ImportSummary { imported, skipped, .. }) => Ok(SyncReport {
            imported,
            skipped,
            errors: Vec::new(),
        }),
        Err(e) => Ok(SyncReport {
            imported: 0,
            skipped: 0,
            errors: vec![e],
        }),
    }
}

fn collect_stats() -> Result<MemoryStats, String> {
    let sources = default_sources(None, None);
    let mut out: Vec<SourceStats> = Vec::with_capacity(sources.len());
    let mut total_file_count = 0usize;
    let mut total_bytes: u64 = 0;

    for src in &sources {
        let stats = stats_for_source(src);
        total_file_count += stats.file_count;
        total_bytes = total_bytes.saturating_add(stats.total_bytes);
        out.push(stats);
    }

    Ok(MemoryStats {
        sources: out,
        chroma: chroma_state(),
        total_file_count,
        total_bytes,
    })
}

fn stats_for_source(src: &MemorySource) -> SourceStats {
    // `walk_markdown` handles the file-vs-dir branch and the 1 MiB ceiling
    // exactly like our other readers — match it so totals here line up with
    // what `walk_markdown` consumers (memory search, dedupe) actually see.
    let files = walk_markdown(src);
    let mut total_bytes: u64 = 0;
    let mut oldest: Option<i64> = None;
    let mut newest: Option<i64> = None;

    for path in &files {
        let Ok(meta) = fs::metadata(path) else { continue };
        total_bytes = total_bytes.saturating_add(meta.len());
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                let ms = dur.as_millis() as i64;
                oldest = Some(match oldest { Some(o) => o.min(ms), None => ms });
                newest = Some(match newest { Some(n) => n.max(ms), None => ms });
            }
        }
    }

    SourceStats {
        label: src.label.clone(),
        kind: src.kind,
        root_path: src.root.display().to_string(),
        file_count: files.len(),
        total_bytes,
        oldest_unix_ms: oldest,
        newest_unix_ms: newest,
    }
}

fn chroma_state() -> ChromaState {
    let Some(home) = dirs::home_dir() else { return ChromaState::default() };
    let path: &Path = &home.join(".claude-mem").join("chroma").join("chroma.sqlite3");
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => ChromaState { exists: true, bytes: meta.len() },
        _ => ChromaState::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_stats_never_errors() {
        // No matter the host filesystem, `collect_stats` should at least
        // return an empty-but-valid struct. Smoke test only.
        let stats = collect_stats().expect("stats should succeed");
        // Totals are coherent — sum of per-source matches top-line.
        let sum: u64 = stats.sources.iter().map(|s| s.total_bytes).sum();
        assert_eq!(sum, stats.total_bytes);
        let n: usize = stats.sources.iter().map(|s| s.file_count).sum();
        assert_eq!(n, stats.total_file_count);
    }

    #[test]
    fn chroma_state_is_consistent() {
        let s = chroma_state();
        // If it doesn't exist, bytes must be zero — the struct should never
        // report a non-zero size for a missing file.
        if !s.exists {
            assert_eq!(s.bytes, 0);
        }
    }
}
