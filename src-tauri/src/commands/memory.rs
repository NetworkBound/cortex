use crate::app_state::AppState;
use crate::memory::{chroma, markdown, sources};
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct MemoryFile {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub source: String,
    pub source_kind: String,
    pub modified_unix_ms: i64,
}

/// Resolve the effective Obsidian vault path: explicit argument first, then
/// the live `AppState` config (auto-detected `Documents/Cortex Brain` on
/// first run, or the user-set vault from Settings). Returning `None` means
/// no vault is active and Obsidian sources will be skipped.
fn resolve_vault(arg: Option<String>, state: &State<'_, AppState>) -> Option<PathBuf> {
    if let Some(s) = arg {
        if !s.trim().is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    state.config.read().obsidian_vault.clone()
}

/// Wave 232 — diagnostic exposed via `/memory-paths` slash. Returns the
/// MemorySource list (label, root, kind) without walking the files.
/// Lightweight so users can verify which roots are active without
/// paying the indexing cost.
#[derive(serde::Serialize)]
pub struct MemorySourceInfo {
    pub label: String,
    pub root: String,
    pub kind: String,
}

#[tauri::command]
pub async fn list_memory_sources(
    active_project: Option<String>,
    obsidian_vault: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<MemorySourceInfo>, String> {
    let active = active_project.as_ref().map(PathBuf::from);
    let vault = resolve_vault(obsidian_vault, &state);
    let srcs = sources::default_sources(active.as_deref(), vault.as_deref());
    Ok(srcs
        .into_iter()
        .map(|s| MemorySourceInfo {
            label: s.label,
            root: s.root.display().to_string(),
            kind: serde_json::to_value(&s.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".into()),
        })
        .collect())
}

#[tauri::command]
pub async fn list_memory_files(
    active_project: Option<String>,
    obsidian_vault: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<MemoryFile>, String> {
    let active = active_project.as_ref().map(PathBuf::from);
    let vault = resolve_vault(obsidian_vault, &state);
    let srcs = sources::default_sources(active.as_deref(), vault.as_deref());

    let mut out = Vec::new();
    for src in &srcs {
        for p in sources::walk_markdown(src) {
            let meta = std::fs::metadata(&p).ok();
            let modified = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(MemoryFile {
                name: p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                source: src.label.clone(),
                // Serde-driven snake_case ("ClaudeProjectMemory" →
                // "claude_project_memory") so the frontend filter tabs
                // ("Auto-memory" / "Project" / "Global") actually match.
                // `Debug` would produce "claudeprojectmemory" instead.
                source_kind: serde_json::to_value(&src.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                modified_unix_ms: modified,
                path: p.display().to_string(),
            });
        }
    }

    out.sort_by(|a, b| b.modified_unix_ms.cmp(&a.modified_unix_ms));
    Ok(out)
}

#[tauri::command]
pub async fn get_memory_entry(path: String) -> Result<markdown::MarkdownEntry, String> {
    markdown::read_entry(&PathBuf::from(path)).map_err(|e| e.to_string())
}

/// Largest byte index `<= idx` that lies on a UTF-8 char boundary (and `<=
/// s.len()`). Stable-Rust stand-in for the unstable `str::floor_char_boundary`,
/// used to keep snippet slices from panicking mid-codepoint.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[derive(Debug, Serialize)]
pub struct MemorySearchHit {
    pub source: String,
    pub path: String,
    pub snippet: String,
    pub score: i32,
}

#[tauri::command]
pub async fn search_memory(
    query: String,
    active_project: Option<String>,
    obsidian_vault: Option<String>,
    include_chroma: bool,
    state: State<'_, AppState>,
) -> Result<Vec<MemorySearchHit>, String> {
    let q = query.to_lowercase();
    if q.trim().is_empty() {
        return Ok(Vec::new());
    }

    let active = active_project.as_ref().map(PathBuf::from);
    let vault = resolve_vault(obsidian_vault, &state);
    let srcs = sources::default_sources(active.as_deref(), vault.as_deref());

    let mut hits: Vec<MemorySearchHit> = Vec::new();
    for src in &srcs {
        for path in sources::walk_markdown(src) {
            let Ok(body) = std::fs::read_to_string(&path) else { continue };
            let body_lc = body.to_lowercase();
            let mut score = 0;
            let mut idx = 0;
            while let Some(pos) = body_lc[idx..].find(&q) {
                score += 1;
                idx += pos + q.len();
                if score > 20 { break; }
            }
            if score == 0 { continue; }
            // Snippet offsets are computed from `body_lc`, so they must slice
            // `body_lc` too — lowercasing can change byte lengths, so reusing
            // them against `body` can land mid-codepoint and panic. Clamp both
            // ends to char boundaries for safety even on `body_lc`.
            let first = body_lc.find(&q).unwrap_or(0);
            let start = floor_char_boundary(&body_lc, first.saturating_sub(60));
            let end = floor_char_boundary(&body_lc, (first + q.len() + 100).min(body_lc.len()));
            let snippet = body_lc[start..end].replace('\n', " ");
            hits.push(MemorySearchHit {
                source: src.label.clone(),
                path: path.display().to_string(),
                snippet,
                score,
            });
        }
    }

    if include_chroma {
        if let Ok(rows) = chroma::substring_search(&query, 10) {
            for row in rows {
                hits.push(MemorySearchHit {
                    source: "chroma".into(),
                    path: row.id,
                    snippet: row.document.chars().take(200).collect(),
                    score: 1,
                });
            }
        }
    }

    hits.sort_by(|a, b| b.score.cmp(&a.score));
    hits.truncate(50);
    Ok(hits)
}

#[tauri::command]
pub async fn write_memory_entry(path: String, content: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, content).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn create_memory_entry(path: String, content: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if p.exists() {
        return Err(format!("file already exists: {}", path));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, content).map_err(|e| e.to_string())?;
    Ok(())
}
