//! Disk-backed mirror of the frontend's localStorage UI prefs.
//!
//! `clear_all_browsing_data()` (lib.rs) wipes localStorage on every app-version
//! change to bust stale webview caches — collateral damage being the user's
//! prefs (onboarding flag, sidebar/nav widths, prompt history, theme, ...). The
//! per-launch wipe was already gated (commit df0356e); this pair lets the
//! frontend mirror those prefs to `~/.cortex/ui-prefs.json` and restore them
//! after a post-update wipe, so prefs survive updates too.

use std::path::PathBuf;

fn prefs_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    Ok(home.join(".cortex").join("ui-prefs.json"))
}

/// Read the persisted prefs blob. Returns `"{}"` when nothing has been written
/// yet so the caller can always `JSON.parse` the result.
#[tauri::command]
pub fn read_ui_prefs() -> Result<String, String> {
    let path = prefs_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}".to_string()),
        Err(e) => Err(format!("read ui-prefs: {e}")),
    }
}

/// Persist the prefs blob via a temp-file + atomic rename. Rejects non-JSON so a
/// corrupt write can never poison the next restore.
#[tauri::command]
pub fn write_ui_prefs(json: String) -> Result<(), String> {
    serde_json::from_str::<serde_json::Value>(&json).map_err(|e| format!("invalid json: {e}"))?;
    let path = prefs_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}
