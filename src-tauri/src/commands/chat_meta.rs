//! Per-chat metadata (custom titles, favorites, tags) persisted as a JSON
//! object at `~/.cortex/chat-meta.json`. The store is keyed by the chat
//! transcript's absolute `file_path` (same identifier used by
//! `chat_history::list_claude_chats`), so renaming a chat in the sidebar
//! never touches the underlying transcript file.
//!
//! Shape on disk:
//! ```json
//! { "/abs/path/to/chat.jsonl": { "custom_title": "Cortex sidebar pass",
//!                                "is_favorite": true,
//!                                "tags": ["cortex", "ui"] } }
//! ```
//!
//! Reads degrade to "empty map" on a missing or corrupt file so a fresh
//! install doesn't have to seed anything. Writes use read-modify-write —
//! the file is tiny and only mutated from `set_chat_meta`, so an in-process
//! mutex (no on-disk lock) is sufficient.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// Per-chat metadata as stored on disk and surfaced to the sidebar. All
/// fields are optional/defaulted so older `chat-meta.json` files without
/// newer keys keep parsing cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ChatMeta {
    /// `true` when the entry carries no user intent and can be dropped from
    /// the store to keep the file small.
    fn is_empty(&self) -> bool {
        self.custom_title.as_deref().unwrap_or("").is_empty()
            && !self.is_favorite
            && self.tags.is_empty()
    }
}

/// Serialized writes guard so concurrent `set_chat_meta` calls from the
/// sidebar's star + rename flows don't race the JSON file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("chat-meta.json"))
}

fn load_all() -> HashMap<String, ChatMeta> {
    let Ok(path) = store_path() else {
        return HashMap::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return HashMap::new();
    };
    serde_json::from_slice::<HashMap<String, ChatMeta>>(&bytes).unwrap_or_default()
}

fn save_all(map: &HashMap<String, ChatMeta>) -> Result<(), String> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(map).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

/// Trim/cap fields so a hostile or oversized payload can't bloat the JSON
/// file. Empty tags are dropped, tag count is capped at 16, title at 256
/// chars to match the bookmarks store conventions.
fn sanitize(mut m: ChatMeta) -> ChatMeta {
    m.custom_title = m
        .custom_title
        .map(|s| s.trim().chars().take(256).collect::<String>())
        .filter(|s| !s.is_empty());
    m.tags = m
        .tags
        .into_iter()
        .map(|t| t.trim().chars().take(64).collect::<String>())
        .filter(|t| !t.is_empty())
        .take(16)
        .collect();
    m
}

#[tauri::command]
pub async fn get_chat_meta(file_path: String) -> Result<Option<ChatMeta>, String> {
    if file_path.trim().is_empty() {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let map = load_all();
        Ok::<Option<ChatMeta>, String>(map.get(&file_path).cloned())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn set_chat_meta(file_path: String, meta: ChatMeta) -> Result<(), String> {
    if file_path.trim().is_empty() {
        return Err("file_path is required".to_string());
    }
    let cleaned = sanitize(meta);
    tokio::task::spawn_blocking(move || {
        let _guard = WRITE_LOCK.lock().map_err(|e| format!("lock poisoned: {e}"))?;
        let mut map = load_all();
        if cleaned.is_empty() {
            map.remove(&file_path);
        } else {
            map.insert(file_path, cleaned);
        }
        save_all(&map)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn list_chat_meta() -> Result<HashMap<String, ChatMeta>, String> {
    tokio::task::spawn_blocking(|| Ok::<HashMap<String, ChatMeta>, String>(load_all()))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_when_all_defaults() {
        let m = ChatMeta::default();
        assert!(m.is_empty());
    }

    #[test]
    fn not_empty_with_favorite() {
        let m = ChatMeta {
            is_favorite: true,
            ..Default::default()
        };
        assert!(!m.is_empty());
    }

    #[test]
    fn sanitize_trims_title_and_tags() {
        let m = sanitize(ChatMeta {
            custom_title: Some("   hello   ".into()),
            is_favorite: false,
            tags: vec!["  one ".into(), "".into(), "two".into()],
        });
        assert_eq!(m.custom_title.as_deref(), Some("hello"));
        assert_eq!(m.tags, vec!["one".to_string(), "two".into()]);
    }

    #[test]
    fn sanitize_caps_tag_count() {
        let many: Vec<String> = (0..40).map(|i| format!("t{i}")).collect();
        let m = sanitize(ChatMeta {
            custom_title: None,
            is_favorite: true,
            tags: many,
        });
        assert_eq!(m.tags.len(), 16);
    }

    #[test]
    fn sanitize_drops_blank_title() {
        let m = sanitize(ChatMeta {
            custom_title: Some("   ".into()),
            is_favorite: true,
            tags: vec![],
        });
        assert!(m.custom_title.is_none());
    }
}
