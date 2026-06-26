//! Tauri commands for the three-tier sandbox.
//!
//! The runtime gate lives in `chat.rs`; these commands only let the UI read
//! and persist the configured tier per project.

use std::path::PathBuf;

use crate::orchestrator::{is_read_only_command, load_tier, write_tier, SandboxTier};

/// Result of classifying a shell command for the UI (e.g. the `/run` composer
/// or an approval prompt): is it provably read-only, with a one-line reason?
#[derive(Debug, serde::Serialize)]
pub struct CommandClassification {
    pub read_only: bool,
    pub reason: String,
}

/// Codex-style classification of a shell command line. A `read_only:true`
/// command is safe to run under any sandbox tier (it cannot mutate the
/// filesystem, network, or process state); anything else requires
/// `workspace-write`/`danger-full-access` or an explicit approval.
#[tauri::command]
pub async fn classify_shell_command(command: String) -> CommandClassification {
    let read_only = is_read_only_command(&command);
    let reason = if read_only {
        "read-only — inspects only; runs under any sandbox tier".to_string()
    } else {
        "not classified read-only — may modify the workspace; needs workspace-write or approval"
            .to_string()
    };
    CommandClassification { read_only, reason }
}

/// Read the configured sandbox tier for `project_root`. Missing /
/// malformed files yield the default (`workspace-write`).
#[tauri::command]
pub async fn get_sandbox_tier(project_root: String) -> Result<String, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = PathBuf::from(&project_root);
    Ok(load_tier(&root).as_str().to_string())
}

/// Persist the sandbox tier for `project_root`. Accepted values:
/// `"read-only"`, `"workspace-write"`, `"danger-full-access"` (plus snake/
/// camel variants — see `SandboxTier::parse`).
#[tauri::command]
pub async fn set_sandbox_tier(project_root: String, tier: String) -> Result<(), String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let parsed = SandboxTier::parse(&tier)
        .ok_or_else(|| format!("invalid tier '{tier}' (use read-only|workspace-write|danger-full-access)"))?;
    let root = PathBuf::from(&project_root);
    write_tier(&root, parsed).map_err(|e| e.to_string())
}
