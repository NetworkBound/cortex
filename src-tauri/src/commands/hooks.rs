//! Tauri commands for the hook event system.
//!
//! Today this is just a read-only listing for the Workspace settings tab —
//! users edit `.cortex/hooks/hooks.json` by hand (mirrors the Claude Code
//! workflow). A future write command can live alongside.

use std::path::PathBuf;

use crate::hooks::HooksConfig;

/// Return the parsed hooks config for `project_root`. Missing or
/// malformed files return an empty config (the read side never errors —
/// hooks are an opt-in feature). The renderer only needs this to render
/// "what's configured" badges so a hard error would be hostile.
#[tauri::command]
pub async fn list_hooks(project_root: String) -> Result<HooksConfig, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = PathBuf::from(&project_root);
    Ok(HooksConfig::load(&root))
}
