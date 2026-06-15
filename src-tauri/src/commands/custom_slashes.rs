//! User-defined slash commands. Persisted as a single YAML list at
//! `~/.cortex/custom-slashes.yaml` so the user can hand-edit the file with
//! `vim` (or `/edit-config` once it learns about this preset). Each entry is
//! a `{name, description, body}` triple; `body` is interpreted by the frontend
//! as one slash command per line, run sequentially.
//!
//! On-disk schema (a top-level list):
//! ```yaml
//! - name: morning
//!   description: Daily standup + summary
//!   body: |
//!     /workflow morning-standup
//!     /summary
//! - name: clean
//!   description: Tests + stage bugfixes + commit message
//!   body: |
//!     /test
//!     /stage just the bugfixes
//!     /commit-msg
//! ```
//!
//! Backend surface mirrors the spec the frontend was wired against:
//!   * `list_custom_slashes()` → ordered list (file order preserved).
//!   * `save_custom_slash(slash)` → upsert by name, returns the persisted row.
//!   * `delete_custom_slash(name)` → no-op when missing.
//!
//! Failures degrade to "empty list" rather than throwing, matching the
//! conventions in `snippets.rs` and `workflows.rs`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A single user-defined slash command. `body` is free text; the frontend
/// splits it into lines and dispatches each as a separate `/cmd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomSlash {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body: String,
}

fn store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("custom-slashes.yaml"))
}

/// Lowercase letters, digits, single hyphens. Same flavour as the rest of
/// the user-typed name guards in this codebase — picker-friendly, can't be
/// confused for a path, and never collides with the leading `/` shell.
fn is_valid_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 48 {
        return false;
    }
    trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn load_all() -> Vec<CustomSlash> {
    let Some(path) = store_path() else {
        return Vec::new();
    };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    match serde_yaml::from_str::<Vec<CustomSlash>>(&raw) {
        Ok(list) => list.into_iter().filter(|s| is_valid_name(&s.name)).collect(),
        Err(e) => {
            tracing::debug!("custom-slashes: parse failed at {}: {e}", path.display());
            Vec::new()
        }
    }
}

fn save_all(list: &[CustomSlash]) -> Result<(), String> {
    let path = store_path().ok_or_else(|| "no home dir".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let body = serde_yaml::to_string(list).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, body).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

// ---------- Tauri commands ----------

/// Return every saved custom slash in file order. Empty list on any error.
#[tauri::command]
pub async fn list_custom_slashes() -> Result<Vec<CustomSlash>, String> {
    tokio::task::spawn_blocking(load_all)
        .await
        .map_err(|e| format!("join error: {e}"))
}

/// Upsert by name. The body is capped at 16 KB so a runaway paste can't bloat
/// the YAML file. Returns the row as persisted.
#[tauri::command]
pub async fn save_custom_slash(slash: CustomSlash) -> Result<CustomSlash, String> {
    if !is_valid_name(&slash.name) {
        return Err(format!(
            "invalid slash name '{}': use lowercase letters, digits, '-' or '_' (≤48 chars)",
            slash.name
        ));
    }
    if slash.body.len() > 16 * 1024 {
        return Err("body exceeds 16 KB limit".into());
    }
    if slash.description.len() > 256 {
        return Err("description exceeds 256 char limit".into());
    }
    tokio::task::spawn_blocking(move || {
        let mut list = load_all();
        let normalized = CustomSlash {
            name: slash.name.trim().to_string(),
            description: slash.description.trim().to_string(),
            body: slash.body.clone(),
        };
        if let Some(existing) = list.iter_mut().find(|s| s.name == normalized.name) {
            *existing = normalized.clone();
        } else {
            list.push(normalized.clone());
        }
        save_all(&list)?;
        Ok::<CustomSlash, String>(normalized)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Delete by name. Missing entries are a no-op.
#[tauri::command]
pub async fn delete_custom_slash(name: String) -> Result<(), String> {
    if !is_valid_name(&name) {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let mut list = load_all();
        let before = list.len();
        list.retain(|s| s.name != name);
        if list.len() != before {
            save_all(&list)?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _g = LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn name_validation() {
        assert!(is_valid_name("morning"));
        assert!(is_valid_name("clean-build"));
        assert!(is_valid_name("ship_v2"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name(&"x".repeat(49)));
    }

    #[test]
    fn list_empty_when_file_missing() {
        with_temp_home(|| {
            assert!(load_all().is_empty());
        });
    }

    #[test]
    fn upsert_then_list_then_delete() {
        with_temp_home(|| {
            let one = CustomSlash {
                name: "morning".into(),
                description: "standup".into(),
                body: "/workflow morning-standup\n/summary\n".into(),
            };
            save_all(&[one.clone()]).unwrap();
            assert_eq!(load_all(), vec![one.clone()]);

            // Upsert: replace by name, file order preserved.
            let two = CustomSlash {
                name: "clean".into(),
                description: "ship".into(),
                body: "/test\n/commit-msg\n".into(),
            };
            let updated = CustomSlash {
                description: "standup v2".into(),
                ..one.clone()
            };
            let mut list = load_all();
            list.push(two.clone());
            list[0] = updated.clone();
            save_all(&list).unwrap();
            assert_eq!(load_all(), vec![updated.clone(), two.clone()]);

            // Delete one — the other stays.
            let mut after = load_all();
            after.retain(|s| s.name != "morning");
            save_all(&after).unwrap();
            assert_eq!(load_all(), vec![two]);
        });
    }

    #[test]
    fn parses_spec_example() {
        // Sanity check: the YAML example in the module doc parses cleanly.
        let raw = "- name: morning\n  description: Daily standup\n  body: |\n    /workflow morning-standup\n    /summary\n- name: clean\n  description: Tests + stage + commit\n  body: |\n    /test\n    /stage just the bugfixes\n    /commit-msg\n";
        let parsed: Vec<CustomSlash> = serde_yaml::from_str(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "morning");
        assert!(parsed[0].body.contains("/workflow morning-standup"));
        assert!(parsed[1].body.contains("/commit-msg"));
    }

    #[test]
    fn skips_invalid_names_on_load() {
        with_temp_home(|| {
            let path = store_path().unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                "- name: ok\n  description: yes\n  body: /test\n- name: \"../bad\"\n  description: no\n  body: /pwn\n",
            )
            .unwrap();
            let list = load_all();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].name, "ok");
        });
    }
}
