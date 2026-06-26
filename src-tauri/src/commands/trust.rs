//! Tauri commands for the global project-trust list.
//!
//! Trust is **global**, not per-project — the list lives at
//! `~/.cortex/trusted-paths.json` and is shared across every Cortex window
//! and session. See `orchestrator::trust` for the storage layer.
//!
//! Untrusted is the default state. The UI is expected to:
//!   * Call `get_trust_status` on project switch.
//!   * Show a banner offering "Trust this project" when status is `false`.
//!   * Call `trust_project` when the user confirms.
//!
//! Untrusted projects have their sandbox tier forced to `read-only` in
//! `commands/chat.rs` and their `.cortex/rules/*.md` skipped in
//! `commands/sessions.rs::gather_project_context`.

use std::path::PathBuf;

use crate::orchestrator::trust;

fn parse_root(project_root: &str) -> Result<PathBuf, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    Ok(PathBuf::from(project_root))
}

/// Returns `true` iff `project_root` is in `~/.cortex/trusted-paths.json`.
/// Missing file / unknown path → `false` (deny-bias default).
#[tauri::command]
pub async fn get_trust_status(project_root: String) -> Result<bool, String> {
    let root = parse_root(&project_root)?;
    Ok(trust::is_trusted(&root))
}

/// Add `project_root` to the global trust list. Idempotent.
#[tauri::command]
pub async fn trust_project(project_root: String) -> Result<(), String> {
    let root = parse_root(&project_root)?;
    trust::trust_path(&root).map_err(|e| e.to_string())
}

/// Remove `project_root` from the global trust list. Idempotent.
#[tauri::command]
pub async fn untrust_project(project_root: String) -> Result<(), String> {
    let root = parse_root(&project_root)?;
    trust::untrust_path(&root).map_err(|e| e.to_string())
}

// ── Cline-style granular trust matrix ────────────────────────────────────
//
// A separate, simpler store from the project-trust list above: just an
// 8-toggle policy + `max_requests_per_task` cap, persisted at
// `~/.cortex/trust-matrix.json`. The UI panel (`TrustMatrix.tsx`) reads it
// at mount and writes back on every change; the matrix is applied at
// approval time by other modules — we just own the storage layer here.
//
// Missing / corrupt file → defaults (everything off, cap = 20). This keeps
// the deny-bias consistent with the project-trust default.

use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustMatrix {
    #[serde(default)]
    pub read_in_workspace: bool,
    #[serde(default)]
    pub read_outside: bool,
    #[serde(default)]
    pub edit_in_workspace: bool,
    #[serde(default)]
    pub edit_outside: bool,
    #[serde(default)]
    pub safe_commands: bool,
    #[serde(default)]
    pub all_commands: bool,
    #[serde(default)]
    pub browser: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default = "default_max_requests")]
    pub max_requests_per_task: u32,
}

fn default_max_requests() -> u32 {
    20
}

impl Default for TrustMatrix {
    fn default() -> Self {
        Self {
            read_in_workspace: false,
            read_outside: false,
            edit_in_workspace: false,
            edit_outside: false,
            safe_commands: false,
            all_commands: false,
            browser: false,
            mcp: false,
            max_requests_per_task: default_max_requests(),
        }
    }
}

fn trust_matrix_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("trust-matrix.json"))
}

#[tauri::command]
pub async fn get_trust_matrix() -> Result<TrustMatrix, String> {
    tokio::task::spawn_blocking(|| {
        let Ok(path) = trust_matrix_path() else {
            return TrustMatrix::default();
        };
        let Ok(bytes) = fs::read(&path) else {
            return TrustMatrix::default();
        };
        serde_json::from_slice::<TrustMatrix>(&bytes).unwrap_or_default()
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn set_trust_matrix(matrix: TrustMatrix) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = trust_matrix_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        let json =
            serde_json::to_vec_pretty(&matrix).map_err(|e| format!("serialize failed: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}
