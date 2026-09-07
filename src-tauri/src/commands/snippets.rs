//! Saved-prompt snippet store, persisted as a single JSON map at
//! `~/.cortex/snippets.json`. Simple read-modify-write semantics — the file is
//! tiny (a few KB at most) and never written from background workers, so we
//! don't bother with a lock.
//!
//! Frontend surface lives in `src/lib/snippets.ts`. The composer uses these
//! commands to populate the `#`-trigger picker; the resulting `#snippet:name`
//! markers are expanded back into snippet bodies before `chat_send` fires.
//!
//! Errors degrade to "empty result" rather than panic: a missing or corrupt
//! `snippets.json` is treated as "no snippets yet", so a fresh install
//! doesn't have to seed the file.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// On-disk record. The `name` field is *not* serialized — it's the map key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSnippet {
    pub body: String,
    #[serde(default)]
    pub created_unix_ms: i64,
    #[serde(default)]
    pub last_used_unix_ms: i64,
}

/// Outbound shape: same fields as `StoredSnippet` plus the name. Matches the
/// `Snippet` TS interface in `src/lib/snippets.ts`.
#[derive(Debug, Clone, Serialize)]
pub struct Snippet {
    pub name: String,
    pub body: String,
    pub created_unix_ms: i64,
    pub last_used_unix_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn snippets_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("snippets.json"))
}

fn load_map() -> BTreeMap<String, StoredSnippet> {
    let Ok(path) = snippets_path() else {
        return BTreeMap::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice::<BTreeMap<String, StoredSnippet>>(&bytes).unwrap_or_default()
}

fn save_map(map: &BTreeMap<String, StoredSnippet>) -> Result<(), String> {
    let path = snippets_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(map).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

/// Snippet names are user-typed, so guard against path-traversal-ish weirdness
/// and keep them picker-friendly: letters, digits, `-`, `_`, `.`.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn to_out(name: String, s: StoredSnippet) -> Snippet {
    Snippet {
        name,
        body: s.body,
        created_unix_ms: s.created_unix_ms,
        last_used_unix_ms: s.last_used_unix_ms,
    }
}

#[tauri::command]
pub async fn list_snippets() -> Result<Vec<Snippet>, String> {
    tokio::task::spawn_blocking(|| {
        let map = load_map();
        let mut out: Vec<Snippet> = map
            .into_iter()
            .map(|(name, s)| to_out(name, s))
            .collect();
        // Most-recently-used first so the picker surfaces hot snippets fast.
        out.sort_by(|a, b| b.last_used_unix_ms.cmp(&a.last_used_unix_ms));
        out
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn get_snippet(name: String) -> Result<Option<Snippet>, String> {
    if !is_valid_name(&name) {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let mut map = load_map();
        let Some(stored) = map.get(&name).cloned() else {
            return Ok::<Option<Snippet>, String>(None);
        };
        // Bump `last_used` on read so list ordering reflects actual usage.
        if let Some(entry) = map.get_mut(&name) {
            entry.last_used_unix_ms = now_ms();
        }
        // Best-effort write — if it fails, still return the snippet.
        let _ = save_map(&map);
        Ok(Some(to_out(
            name.clone(),
            StoredSnippet {
                last_used_unix_ms: now_ms(),
                ..stored
            },
        )))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn save_snippet(name: String, body: String) -> Result<Snippet, String> {
    if !is_valid_name(&name) {
        return Err(format!(
            "invalid snippet name '{name}': use letters, digits, _, -, ."
        ));
    }
    if body.len() > 64 * 1024 {
        return Err("snippet body exceeds 64 KB limit".to_string());
    }
    tokio::task::spawn_blocking(move || {
        let mut map = load_map();
        let now = now_ms();
        let created = map
            .get(&name)
            .map(|s| s.created_unix_ms)
            .filter(|t| *t > 0)
            .unwrap_or(now);
        let stored = StoredSnippet {
            body,
            created_unix_ms: created,
            last_used_unix_ms: now,
        };
        map.insert(name.clone(), stored.clone());
        save_map(&map)?;
        Ok::<Snippet, String>(to_out(name, stored))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn delete_snippet(name: String) -> Result<(), String> {
    if !is_valid_name(&name) {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let mut map = load_map();
        if map.remove(&name).is_some() {
            save_map(&map)?;
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
        assert!(is_valid_name("foo"));
        assert!(is_valid_name("foo-bar.v2"));
        assert!(is_valid_name("a_b_c"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name("a/b"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }
}
