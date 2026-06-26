//! Bookmarks / favorites store, persisted as a JSON array at
//! `~/.cortex/bookmarks.json`. Users can ⭐ pin any kind of Cortex artefact —
//! a memory entry, a file path, a trace id, a chat session, an arbitrary URL,
//! or just a free-form note — to a quick-access list.
//!
//! Simple read-modify-write semantics; the file is small (a handful of KB at
//! worst) and only mutated from these commands, so no on-disk lock is needed.
//! Errors degrade to "empty list" rather than panicking: a missing or corrupt
//! `bookmarks.json` is treated as "no bookmarks yet" so a fresh install
//! doesn't have to seed the file.
//!
//! Frontend surface lives in `src/lib/bookmarks.ts`. The activity panel's
//! `BookmarksPanel.tsx` is the primary view; the chat `/bookmark` and
//! `/bookmarks` slash commands provide a keyboard-driven entry point.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Allowed `kind` values, mirrored in `src/lib/bookmarks.ts`. We keep the
/// allow-list short and validate at the boundary so a malformed `bookmarks.json`
/// doesn't smuggle unexpected variants into the UI.
const VALID_KINDS: &[&str] = &["memory", "file", "trace", "session", "url", "note"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub target: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub created_unix_ms: i64,
    #[serde(default)]
    pub last_opened_unix_ms: Option<i64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn bookmarks_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("bookmarks.json"))
}

fn load_all() -> Vec<Bookmark> {
    let Ok(path) = bookmarks_path() else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<Bookmark>>(&bytes).unwrap_or_default()
}

fn save_all(items: &[Bookmark]) -> Result<(), String> {
    let path = bookmarks_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(items).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

fn is_valid_kind(kind: &str) -> bool {
    VALID_KINDS.iter().any(|k| *k == kind)
}

/// Clamp + sanitize so the panel never has to re-validate inbound data.
/// Strips leading/trailing whitespace from labels/targets/tags and drops
/// empty tags. Label/target/note length caps avoid runaway JSON files.
fn sanitize(mut b: Bookmark) -> Bookmark {
    b.label = b.label.trim().chars().take(256).collect();
    b.target = b.target.trim().chars().take(2048).collect();
    b.tags = b
        .tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(16)
        .collect();
    b.note = b
        .note
        .map(|n| n.trim().chars().take(512).collect::<String>())
        .filter(|n| !n.is_empty());
    b
}

#[tauri::command]
pub async fn list_bookmarks(
    filter_kind: Option<String>,
    filter_tag: Option<String>,
) -> Result<Vec<Bookmark>, String> {
    tokio::task::spawn_blocking(move || {
        let mut items = load_all();
        if let Some(k) = filter_kind.as_deref() {
            items.retain(|b| b.kind == k);
        }
        if let Some(t) = filter_tag.as_deref() {
            items.retain(|b| b.tags.iter().any(|x| x == t));
        }
        // Most-recently-opened first, then most-recently-created. Keeps hot
        // bookmarks at the top of the panel without forcing an explicit sort.
        items.sort_by(|a, b| {
            b.last_opened_unix_ms
                .unwrap_or(0)
                .cmp(&a.last_opened_unix_ms.unwrap_or(0))
                .then_with(|| b.created_unix_ms.cmp(&a.created_unix_ms))
        });
        Ok::<Vec<Bookmark>, String>(items)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn add_bookmark(bookmark: Bookmark) -> Result<Bookmark, String> {
    if !is_valid_kind(&bookmark.kind) {
        return Err(format!(
            "invalid bookmark kind '{}'; expected one of {VALID_KINDS:?}",
            bookmark.kind
        ));
    }
    let mut bm = sanitize(bookmark);
    if bm.label.is_empty() {
        return Err("bookmark label cannot be empty".to_string());
    }
    if bm.target.is_empty() {
        return Err("bookmark target cannot be empty".to_string());
    }
    if bm.id.trim().is_empty() {
        bm.id = ulid::Ulid::new().to_string();
    }
    if bm.created_unix_ms <= 0 {
        bm.created_unix_ms = now_ms();
    }
    tokio::task::spawn_blocking(move || {
        let mut items = load_all();
        // Same id collision → overwrite in place. Different id but same
        // (kind,target) pair → upsert so re-bookmarking the same file
        // doesn't litter the list with dupes.
        let existing = items
            .iter()
            .position(|b| b.id == bm.id || (b.kind == bm.kind && b.target == bm.target));
        match existing {
            Some(idx) => {
                let preserved_id = items[idx].id.clone();
                let preserved_created = items[idx].created_unix_ms;
                items[idx] = Bookmark {
                    id: preserved_id,
                    created_unix_ms: preserved_created,
                    ..bm
                };
                save_all(&items)?;
                Ok::<Bookmark, String>(items[idx].clone())
            }
            None => {
                items.push(bm.clone());
                save_all(&items)?;
                Ok::<Bookmark, String>(bm)
            }
        }
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn update_bookmark(bookmark: Bookmark) -> Result<(), String> {
    if !is_valid_kind(&bookmark.kind) {
        return Err(format!(
            "invalid bookmark kind '{}'; expected one of {VALID_KINDS:?}",
            bookmark.kind
        ));
    }
    let bm = sanitize(bookmark);
    if bm.id.trim().is_empty() {
        return Err("update_bookmark requires an id".to_string());
    }
    tokio::task::spawn_blocking(move || {
        let mut items = load_all();
        let Some(idx) = items.iter().position(|b| b.id == bm.id) else {
            return Err(format!("bookmark '{}' not found", bm.id));
        };
        // Preserve the original creation timestamp so edits don't reorder
        // the list under the "created" tiebreaker.
        let preserved_created = items[idx].created_unix_ms;
        items[idx] = Bookmark {
            created_unix_ms: preserved_created,
            ..bm
        };
        save_all(&items)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn delete_bookmark(id: String) -> Result<(), String> {
    if id.trim().is_empty() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let mut items = load_all();
        let before = items.len();
        items.retain(|b| b.id != id);
        if items.len() != before {
            save_all(&items)?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn touch_bookmark(id: String) -> Result<(), String> {
    if id.trim().is_empty() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let mut items = load_all();
        let Some(idx) = items.iter().position(|b| b.id == id) else {
            return Ok::<(), String>(());
        };
        items[idx].last_opened_unix_ms = Some(now_ms());
        save_all(&items)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(kind: &str, label: &str, target: &str) -> Bookmark {
        Bookmark {
            id: String::new(),
            kind: kind.to_string(),
            label: label.to_string(),
            target: target.to_string(),
            tags: vec![],
            note: None,
            created_unix_ms: 0,
            last_opened_unix_ms: None,
        }
    }

    #[test]
    fn validates_kind() {
        assert!(is_valid_kind("memory"));
        assert!(is_valid_kind("file"));
        assert!(is_valid_kind("trace"));
        assert!(is_valid_kind("session"));
        assert!(is_valid_kind("url"));
        assert!(is_valid_kind("note"));
        assert!(!is_valid_kind(""));
        assert!(!is_valid_kind("Memory"));
        assert!(!is_valid_kind("project"));
    }

    #[test]
    fn sanitize_trims_and_caps() {
        let b = sanitize(Bookmark {
            id: "x".into(),
            kind: "url".into(),
            label: "   hello   ".into(),
            target: "  https://example.com  ".into(),
            tags: vec!["  work  ".into(), "".into(), "later".into()],
            note: Some("   ".into()),
            created_unix_ms: 0,
            last_opened_unix_ms: None,
        });
        assert_eq!(b.label, "hello");
        assert_eq!(b.target, "https://example.com");
        assert_eq!(b.tags, vec!["work".to_string(), "later".into()]);
        assert!(b.note.is_none());
    }

    #[test]
    fn sanitize_drops_oversized_tags() {
        let many: Vec<String> = (0..32).map(|i| format!("tag{i}")).collect();
        let b = sanitize(Bookmark {
            id: "x".into(),
            kind: "note".into(),
            label: "x".into(),
            target: "x".into(),
            tags: many,
            note: None,
            created_unix_ms: 0,
            last_opened_unix_ms: None,
        });
        assert_eq!(b.tags.len(), 16);
    }

    #[test]
    fn sample_bookmark_compiles() {
        // Smoke test the shape; this matches the TS interface.
        let _ = mk("file", "lib.rs", "/tmp/lib.rs");
    }
}
