//! Workspace export / import.
//!
//! Bundles a portable snapshot of Cortex's user-visible state — gateway
//! connection settings (URL + model, **never the API key**), Obsidian vault
//! pointer, per-project `.cortex/*` config files, and the last 50 sessions'
//! messages — into a single JSON file the user can move between machines or
//! commit to a private git repo as a backup.
//!
//! Schema is versioned (`cortex.workspace.v1`) so future imports can detect
//! format drift and refuse incompatible bundles instead of corrupting state.

use crate::app_state::AppState;
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tauri::State;

const SCHEMA_ID: &str = "cortex.workspace.v1";
const MAX_SESSIONS: usize = 50;
const PROJECT_FILES: &[&str] = &[
    ".cortex/rules.md",
    ".cortex/danger.toml",
    ".cortex/approvals.toml",
    ".cortex/keymap.json",
];

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    pub gateway_base_url: String,
    pub gateway_model: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub obsidian_vault: Option<String>,
    /// Frontend-only state (theme, etc.) lives in localStorage and is not
    /// reachable from Rust; the field is reserved for future use.
    pub theme: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSession {
    pub session_id: String,
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceBundle {
    pub schema: String,
    pub exported_at: i64,
    pub settings: WorkspaceSettings,
    /// project-relative path → raw UTF-8 file contents.
    pub project_files: BTreeMap<String, String>,
    pub sessions: Vec<WorkspaceSession>,
}

#[derive(Debug, Serialize)]
pub struct ExportSummary {
    pub path: String,
    pub sessions_exported: usize,
    pub project_files_exported: usize,
    pub bytes_written: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub settings_applied: bool,
    pub sessions_imported: usize,
    pub project_files_written: usize,
    pub project_files_skipped: usize,
}

#[tauri::command]
pub async fn export_workspace(
    out_path: String,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ExportSummary, String> {
    let settings = {
        let cfg = state.config.read();
        WorkspaceSettings {
            gateway_base_url: cfg.gateway_base_url.clone(),
            gateway_model: cfg.gateway_model.clone(),
            ollama_base_url: cfg.ollama_base_url.clone(),
            ollama_model: cfg.ollama_model.clone(),
            obsidian_vault: cfg.obsidian_vault.as_ref().map(|p| p.display().to_string()),
            theme: None,
        }
    };

    let active_project = state.config.read().default_project_root.clone();
    let project_files = collect_project_files(active_project.as_deref());

    let sessions = collect_recent_sessions(&store, MAX_SESSIONS);

    let bundle = WorkspaceBundle {
        schema: SCHEMA_ID.to_string(),
        exported_at: chrono::Utc::now().timestamp_millis(),
        settings,
        project_files,
        sessions,
    };

    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
    let path = PathBuf::from(&out_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    std::fs::write(&path, &json).map_err(|e| e.to_string())?;

    Ok(ExportSummary {
        path: out_path,
        sessions_exported: bundle.sessions.len(),
        project_files_exported: bundle.project_files.len(),
        bytes_written: json.len(),
    })
}

#[tauri::command]
pub async fn import_workspace(
    bundle_path: String,
    project_root: Option<String>,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ImportSummary, String> {
    let raw = std::fs::read(&bundle_path).map_err(|e| e.to_string())?;
    let bundle: WorkspaceBundle = serde_json::from_slice(&raw)
        .map_err(|e| format!("could not parse bundle: {e}"))?;

    if bundle.schema != SCHEMA_ID {
        return Err(format!(
            "unsupported bundle schema '{}' (expected '{}')",
            bundle.schema, SCHEMA_ID
        ));
    }

    // Apply settings — never overwrite API key, never overwrite obsidian
    // with an empty path (let the user clear that explicitly in Settings).
    {
        let mut cfg = state.config.write();
        cfg.gateway_base_url = bundle.settings.gateway_base_url;
        cfg.gateway_model = bundle.settings.gateway_model;
        cfg.ollama_base_url = bundle.settings.ollama_base_url;
        cfg.ollama_model = bundle.settings.ollama_model;
        if let Some(v) = bundle.settings.obsidian_vault.filter(|s| !s.trim().is_empty()) {
            cfg.obsidian_vault = Some(PathBuf::from(v));
        }
    }

    let target_root = project_root
        .map(PathBuf::from)
        .or_else(|| state.config.read().default_project_root.clone());
    let (written, skipped) = write_project_files(target_root.as_deref(), &bundle.project_files)?;

    let mut sessions_with_writes = 0usize;
    for session in &bundle.sessions {
        let mut wrote_any = false;
        for msg in &session.messages {
            // record_message uses INSERT OR REPLACE, so re-import is idempotent.
            if store.record_message(msg).is_ok() {
                wrote_any = true;
            }
        }
        if wrote_any {
            sessions_with_writes += 1;
        }
    }

    Ok(ImportSummary {
        settings_applied: true,
        sessions_imported: sessions_with_writes,
        project_files_written: written,
        project_files_skipped: skipped,
    })
}

/// Read every well-known `.cortex/*` file under the given project root.
/// Missing files are silently skipped. Non-UTF-8 files are also skipped —
/// the bundle format is plain JSON text.
fn collect_project_files(root: Option<&Path>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(root) = root else { return out };
    for rel in PROJECT_FILES {
        let path = root.join(rel);
        if let Ok(body) = std::fs::read_to_string(&path) {
            out.insert((*rel).to_string(), body);
        }
    }
    out
}

/// Returns (written, skipped). A file is skipped when an identical copy
/// already lives on disk — we never clobber user-edited config silently.
fn write_project_files(
    root: Option<&Path>,
    files: &BTreeMap<String, String>,
) -> Result<(usize, usize), String> {
    let Some(root) = root else {
        // No active project — treat all files as skipped rather than failing.
        return Ok((0, files.len()));
    };
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (rel, body) in files {
        // Attacker-controlled keys: reject anything that could escape `root`
        // (absolute paths, `..` components, etc.) before touching the disk.
        let Some(abs) = safe_join(root, rel) else {
            skipped += 1;
            continue;
        };
        if let Ok(existing) = std::fs::read_to_string(&abs) {
            if existing == *body {
                skipped += 1;
                continue;
            }
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            // Re-verify after creating the parent: canonicalizing it resolves
            // any symlinks so we can assert the write target stays under root.
            if let (Ok(canon_parent), Ok(canon_root)) =
                (parent.canonicalize(), root.canonicalize())
            {
                if !canon_parent.starts_with(&canon_root) {
                    skipped += 1;
                    continue;
                }
            }
        }
        std::fs::write(&abs, body).map_err(|e| e.to_string())?;
        written += 1;
    }
    Ok((written, skipped))
}

/// Join an attacker-supplied relative path onto `root`, rejecting any path
/// that is absolute, has a root/prefix component, or contains a `..` (parent)
/// component. Returns `None` if the path is unsafe to write under `root`.
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    use std::path::Component;

    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    for comp in rel_path.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            // RootDir, Prefix (e.g. `C:\`), and ParentDir (`..`) can all
            // escape `root`; refuse them outright.
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => return None,
        }
    }
    Some(root.join(rel_path))
}

fn collect_recent_sessions(store: &TracingStore, limit: usize) -> Vec<WorkspaceSession> {
    let Ok(sessions) = store.recent_sessions(limit) else { return Vec::new() };
    let mut out = Vec::with_capacity(sessions.len());
    for s in sessions {
        let messages = store
            .load_session_messages(&s.session_id)
            .unwrap_or_default();
        if messages.is_empty() {
            continue;
        }
        out.push(WorkspaceSession {
            session_id: s.session_id,
            messages,
        });
    }
    out
}
