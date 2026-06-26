//! Workspace layout presets — save / restore a portable bundle of the
//! UI's current "where am I working?" state in one shot.
//!
//! A preset captures the small set of frontend toggles that together define a
//! workspace mood: active ActivityPanel tab, Cortex `plan`/`act` mode, the
//! sandbox tier in force, the current theme, the gateway model, and the
//! right-column sidebar tab. Restoring a preset reapplies all of those
//! together so the user can flip between, e.g., a "deep-work review" layout
//! and a "ship a hot-fix" layout without click-walking through five menus.
//!
//! Persistence model matches `commands/snippets.rs`: a single JSON file at
//! `~/.cortex/workspace-presets.json`, read-modify-write on every mutation,
//! upsert-by-name semantics. The file is tiny (a few KB at most) and is
//! never touched by background workers, so we don't take a lock.
//!
//! A *missing* presets file is treated as "no presets yet" so a fresh install
//! doesn't have to seed anything. A *corrupt* file degrades to an empty list
//! on the read-only listing path, but causes mutations (`save`/`delete`) to
//! error out instead of overwriting — and thereby erasing — data we couldn't
//! parse.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-wide guard around the read-modify-write cycle on the shared presets
/// file. Mutations (`save`/`delete`) run inside `spawn_blocking` on a thread
/// pool, so two concurrent commands could otherwise each `load_all`, mutate
/// their own copy, and `save_all` — silently dropping the other's change (lost
/// update) or interleaving writes into a corrupt file. Serializing the whole
/// load→modify→save under this lock makes each mutation atomic with respect to
/// the others. The critical section is a few-KB file rewrite, so contention is
/// negligible. We recover from a poisoned lock since the guarded data lives on
/// disk, not in the mutex.
fn presets_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Captured workspace state. Every field is a free-form string so the frontend
/// can extend the vocabulary (new ActivityTab values, new sandbox tiers, …)
/// without a backend schema bump. Validation happens on the apply side in
/// `src/lib/workspace-presets.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePresetState {
    pub activity_tab: Option<String>,
    pub mode: Option<String>,
    pub sandbox_tier: Option<String>,
    pub theme: Option<String>,
    pub gateway_model: Option<String>,
    pub right_tab: Option<String>,
}

/// A single named preset. `created_unix_ms` is set on first save and preserved
/// across upserts so the UI can sort by age.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePreset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub state: WorkspacePresetState,
    #[serde(default)]
    pub created_unix_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn presets_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("workspace-presets.json"))
}

/// Read the presets file. A missing file (or unknown home dir) is "no presets
/// yet" and degrades to an empty list, but a *corrupt* file surfaces as an
/// error: mutating callers must abort rather than silently overwrite — and
/// thereby erase — presets they failed to parse. Read-only callers can choose
/// to treat the error as empty.
fn load_all() -> Result<Vec<WorkspacePreset>, String> {
    let path = presets_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read failed: {e}")),
    };
    serde_json::from_slice::<Vec<WorkspacePreset>>(&bytes)
        .map_err(|e| format!("parse failed: {e}"))
}

fn save_all(items: &[WorkspacePreset]) -> Result<(), String> {
    let path = presets_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(items).map_err(|e| format!("serialize failed: {e}"))?;
    // Write to a sibling temp file then atomically rename into place so a crash
    // (or a racing reader) never observes a half-written presets file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    Ok(())
}

/// Preset names are user-typed — guard against path-traversal-ish weirdness
/// and keep them picker-friendly: letters, digits, `-`, `_`, `.`, space.
fn is_valid_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ' ')
}

#[tauri::command]
pub async fn list_workspace_presets() -> Result<Vec<WorkspacePreset>, String> {
    tokio::task::spawn_blocking(|| {
        // A corrupt/unreadable file degrades to "no presets" for the read-only
        // listing path; the picker just shows empty rather than erroring out.
        let mut items = load_all().unwrap_or_default();
        // Most-recent first so the picker surfaces fresh presets at the top.
        items.sort_by(|a, b| b.created_unix_ms.cmp(&a.created_unix_ms));
        items
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn save_workspace_preset(preset: WorkspacePreset) -> Result<WorkspacePreset, String> {
    if !is_valid_name(&preset.name) {
        return Err(format!(
            "invalid preset name '{}': use letters, digits, _, -, ., space",
            preset.name
        ));
    }
    if preset.description.len() > 280 {
        return Err("description exceeds 280 character limit".to_string());
    }
    tokio::task::spawn_blocking(move || {
        // Serialize the whole read-modify-write so concurrent saves/deletes
        // can't clobber each other (lost update). Recover from poisoning — the
        // source of truth is the file, not the mutex's `()` payload.
        let _guard = presets_lock().lock().unwrap_or_else(|e| e.into_inner());
        // Bail on a corrupt file rather than overwriting it with just this one
        // preset and silently erasing everything else.
        let mut items = load_all()?;
        let name = preset.name.trim().to_string();
        // Preserve the original `created_unix_ms` on upsert so sort order
        // doesn't jump when a user tweaks a long-standing preset.
        let existing_created = items
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.created_unix_ms)
            .filter(|t| *t > 0);
        let stored = WorkspacePreset {
            name: name.clone(),
            description: preset.description,
            state: preset.state,
            created_unix_ms: existing_created.unwrap_or_else(now_ms),
        };
        items.retain(|p| p.name != name);
        items.push(stored.clone());
        save_all(&items)?;
        Ok::<WorkspacePreset, String>(stored)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn delete_workspace_preset(name: String) -> Result<(), String> {
    if !is_valid_name(&name) {
        return Err(format!(
            "invalid preset name '{name}': use letters, digits, _, -, ., space"
        ));
    }
    tokio::task::spawn_blocking(move || {
        // Same lock as `save_workspace_preset` — a delete racing a save on the
        // shared file would otherwise lose one of the two updates.
        let _guard = presets_lock().lock().unwrap_or_else(|e| e.into_inner());
        // Bail on a corrupt file rather than risk clobbering it on the
        // subsequent save_all.
        let mut items = load_all()?;
        let needle = name.trim();
        let before = items.len();
        items.retain(|p| p.name != needle);
        if items.len() != before {
            save_all(&items)?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(is_valid_name("deep-work"));
        assert!(is_valid_name("ship hot-fix"));
        assert!(is_valid_name("v1.2_alpha"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("   "));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name("a/b"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }

    #[test]
    fn round_trip_serde() {
        let p = WorkspacePreset {
            name: "deep-work".into(),
            description: "Plan mode, read-only sandbox".into(),
            state: WorkspacePresetState {
                activity_tab: Some("today".into()),
                mode: Some("plan".into()),
                sandbox_tier: Some("read-only".into()),
                theme: Some("coral-dark".into()),
                gateway_model: Some("claude-opus-4-7".into()),
                right_tab: Some("memory".into()),
            },
            created_unix_ms: 1_716_700_000_000,
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: WorkspacePreset = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, p.name);
        assert_eq!(back.state.activity_tab.as_deref(), Some("today"));
    }
}
