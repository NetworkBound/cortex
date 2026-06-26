//! Per-project thread persistence — parallel chat lanes (Zed `cmd-alt-J`).
//!
//! Each thread is one JSON document stored at
//! `<project_root>/.cortex/threads/<thread_id>.json`. When no project is
//! active the frontend sends the [`GLOBAL_PROJECT_ROOT_KEY`] sentinel, which
//! maps to `~/.cortex/threads/` — global threads persist too. The frontend
//! owns the
//! schema; we only persist + list. We trust the caller to send a sensible
//! `thread.id` (UUIDv4 from `crypto.randomUUID()`); we reject any id that
//! contains a path separator or `..` segment so a malicious thread can't
//! escape the threads directory.
//!
//! Errors are folded into `String` so the frontend's `invoke` boundary stays
//! uniform with every other Cortex Tauri command.
//!
//! Capped at ~5 MB per thread file — anything larger almost certainly
//! indicates a runaway streaming bug; we refuse to load it rather than blow
//! the IPC frame.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_THREAD_BYTES: u64 = 5 * 1024 * 1024;

/// Sentinel sent by the frontend when no project is active. Maps the global
/// thread bucket onto the user's home directory (`~/.cortex/threads/`) so
/// no-project threads persist across restarts instead of silently
/// evaporating. Must stay in sync with `GLOBAL_PROJECT_ROOT_KEY` in
/// `src/lib/threads.ts`.
pub const GLOBAL_PROJECT_ROOT_KEY: &str = "__cortex_global__";

/// Mirror of the frontend `Thread` shape. We pass `messages` through as raw
/// JSON so the Rust side never has to know the full `Message` schema — the
/// frontend evolves it freely without churning this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub label: String,
    /// User-chosen title from the inline rename UI; `None` means the frontend
    /// derives the title from the first message as usual.
    #[serde(rename = "customTitle", default)]
    pub custom_title: Option<String>,
    #[serde(rename = "lastTs")]
    pub last_ts: i64,
    #[serde(default)]
    pub messages: JsonValue,
    #[serde(rename = "runningRunIds", default)]
    pub running_run_ids: Vec<String>,
    #[serde(rename = "lastRoutingReason", default)]
    pub last_routing_reason: Option<String>,
}

fn threads_dir(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("threads")
}

/// Map the frontend's `project_root` argument onto the directory that owns
/// the `.cortex/threads` bucket: the global sentinel resolves to the user's
/// home directory; anything else must be an existing directory.
fn resolve_root(project_root: &str) -> Result<PathBuf, String> {
    if project_root == GLOBAL_PROJECT_ROOT_KEY {
        return dirs::home_dir()
            .ok_or_else(|| "cannot resolve a home directory for global threads".to_string());
    }
    let root = PathBuf::from(project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(root)
}

fn ensure_dir(project_root: &Path) -> Result<PathBuf, String> {
    let dir = threads_dir(project_root);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir threads: {e}"))?;
    Ok(dir)
}

/// Reject ids that could traverse out of the threads directory. UUIDv4 is the
/// expected shape, but we accept any "safe" string (alphanumerics, hyphens,
/// underscores) so a future scheme change doesn't lock us out.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("empty thread id".into());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(format!("unsafe thread id: {id}"));
    }
    for ch in id.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return Err(format!("unsafe thread id char: {ch}"));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn save_thread(project_root: String, thread: Thread) -> Result<(), String> {
    let root = resolve_root(&project_root)?;
    validate_id(&thread.id)?;
    let dir = ensure_dir(&root)?;
    let path = dir.join(format!("{}.json", thread.id));
    let body = serde_json::to_string(&thread).map_err(|e| format!("encode thread: {e}"))?;
    if (body.len() as u64) > MAX_THREAD_BYTES {
        return Err(format!(
            "thread too large ({} bytes) — capped at {}",
            body.len(),
            MAX_THREAD_BYTES
        ));
    }
    // Write to a temp sibling and rename — atomic-ish on POSIX, and on Windows
    // we accept the tiny race window. Avoids torn writes if the app crashes
    // mid-save during a 500-token stream.
    let tmp = dir.join(format!("{}.json.tmp", thread.id));
    fs::write(&tmp, &body).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn list_threads(project_root: String) -> Result<Vec<Thread>, String> {
    // A missing/never-used bucket is a normal "no threads yet" case, not an
    // error — only an unresolvable global root is worth surfacing.
    let root = match resolve_root(&project_root) {
        Ok(r) => r,
        Err(_) if project_root != GLOBAL_PROJECT_ROOT_KEY => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let dir = threads_dir(&root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<Thread> = Vec::new();
    let read = fs::read_dir(&dir).map_err(|e| format!("read dir: {e}"))?;
    for entry in read.flatten() {
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.ends_with(".json.tmp") {
            continue;
        }
        let meta = match fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_THREAD_BYTES {
            tracing::warn!("threads: skipping oversize file {}", p.display());
            continue;
        }
        let body = match fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("threads: read {} failed: {e}", p.display());
                continue;
            }
        };
        match serde_json::from_str::<Thread>(&body) {
            Ok(t) => out.push(t),
            Err(e) => tracing::warn!("threads: parse {} failed: {e}", p.display()),
        }
    }
    // Most-recent first — matches the frontend's natural display order.
    out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    Ok(out)
}

#[tauri::command]
pub async fn delete_thread(project_root: String, id: String) -> Result<(), String> {
    let root = resolve_root(&project_root)?;
    validate_id(&id)?;
    let path = threads_dir(&root).join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("rm thread: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_thread(id: &str, custom_title: Option<&str>, last_ts: i64) -> Thread {
        Thread {
            id: id.to_string(),
            session_id: format!("session-{id}"),
            label: "thread 1".to_string(),
            custom_title: custom_title.map(|s| s.to_string()),
            last_ts,
            messages: serde_json::json!([]),
            running_run_ids: Vec::new(),
            last_routing_reason: None,
        }
    }

    #[test]
    fn validate_id_accepts_uuid_shapes_and_rejects_traversal() {
        assert!(validate_id("thread-123e4567-e89b-42d3-a456-426614174000").is_ok());
        assert!(validate_id("abc_DEF-123").is_ok());
        assert!(validate_id("").is_err());
        assert!(validate_id("../evil").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("a\\b").is_err());
        assert!(validate_id("a b").is_err());
        assert!(validate_id("a.json").is_err()); // '.' is not in the safe set
    }

    #[test]
    fn resolve_root_maps_global_sentinel_to_home() {
        let home = dirs::home_dir().expect("test env has a home dir");
        assert_eq!(resolve_root(GLOBAL_PROJECT_ROOT_KEY).unwrap(), home);
    }

    #[test]
    fn resolve_root_passes_through_real_dirs_and_rejects_missing_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap().to_string();
        assert_eq!(resolve_root(&p).unwrap(), tmp.path());
        assert!(resolve_root("/definitely/not/a/dir/xyz").is_err());
    }

    #[tokio::test]
    async fn save_list_delete_round_trip_preserves_custom_title() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap().to_string();

        save_thread(root.clone(), mk_thread("t-old", None, 100))
            .await
            .unwrap();
        save_thread(root.clone(), mk_thread("t-new", Some("Renamed lane"), 200))
            .await
            .unwrap();

        let listed = list_threads(root.clone()).await.unwrap();
        assert_eq!(listed.len(), 2);
        // Most-recent first.
        assert_eq!(listed[0].id, "t-new");
        assert_eq!(listed[0].custom_title.as_deref(), Some("Renamed lane"));
        assert_eq!(listed[1].id, "t-old");
        assert_eq!(listed[1].custom_title, None);

        delete_thread(root.clone(), "t-new".to_string()).await.unwrap();
        let listed = list_threads(root).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "t-old");
    }

    #[tokio::test]
    async fn list_threads_on_missing_project_dir_is_empty_not_error() {
        let listed = list_threads("/definitely/not/a/dir/xyz".to_string())
            .await
            .unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn thread_json_uses_frontend_field_casing_and_defaults() {
        // A pre-rename thread file (no customTitle key) must still parse.
        let legacy = r#"{"id":"t1","sessionId":"s1","label":"thread 1","lastTs":5}"#;
        let t: Thread = serde_json::from_str(legacy).unwrap();
        assert_eq!(t.custom_title, None);
        assert_eq!(t.last_ts, 5);

        let body = serde_json::to_string(&mk_thread("t2", Some("Title"), 9)).unwrap();
        assert!(body.contains("\"customTitle\":\"Title\""));
        assert!(body.contains("\"sessionId\""));
        assert!(body.contains("\"lastTs\""));
    }
}
