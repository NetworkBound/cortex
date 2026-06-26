//! Tauri command surface for the Skills subsystem.
//!
//! Three commands match the three things the frontend needs:
//!   * `list_skills` — populate the panel on mount.
//!   * `get_skill` — fetch a single skill (the body is what the UI shows in
//!     a preview region).
//!   * `expand_skill` — substitute the user-provided inputs into the body and
//!     return the rendered prompt for injection into chat.
//!
//! All commands hop to `spawn_blocking` because the underlying loader touches
//! the filesystem; we keep parity with the snippets command surface so the
//! same patterns apply across all "user-data file" subsystems.

use std::collections::HashMap;

use crate::skills::{expand_skill as expand_skill_inner, load_skills, skills_root, Skill};

#[tauri::command]
pub async fn list_skills() -> Result<Vec<Skill>, String> {
    tokio::task::spawn_blocking(load_skills)
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn get_skill(name: String) -> Result<Option<Skill>, String> {
    tokio::task::spawn_blocking(move || crate::skills::load_skill_by_name(&name))
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn expand_skill(
    name: String,
    vars: HashMap<String, String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || expand_skill_inner(&name, vars))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Persist a skill authored in the in-app SkillBuilderModal.
///
/// Writes `~/.cortex/skills/<sanitized-name>/SKILL.md` with the YAML frontmatter
/// emitted by the modal followed by the markdown body. Name is sanitized to
/// `[a-z0-9_-]+` so users can't traverse out of the skills dir or shadow
/// system paths. Existing files at that path are overwritten — the loader
/// is the source of truth for what's on disk.
#[tauri::command]
pub async fn save_skill(
    name: String,
    body: String,
    frontmatter: HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let safe: String = name
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        if safe.is_empty() {
            return Err("name is empty after sanitisation".into());
        }
        let root = skills_root().ok_or_else(|| "no home dir".to_string())?;
        let dir = root.join(&safe);
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        // Emit YAML frontmatter when there's anything to write; otherwise
        // just the body so the file stays valid markdown.
        let mut out = String::new();
        if !frontmatter.is_empty() {
            // Stable key order so diffs stay clean.
            let mut keys: Vec<_> = frontmatter.keys().cloned().collect();
            keys.sort();
            // Build a YAML mapping and let serde_yaml handle quoting/escaping.
            // Interpolating values into `{k}: {v}` by hand let a value
            // containing a newline (or other YAML metacharacters) inject
            // arbitrary keys or corrupt the document.
            let mut map = serde_yaml::Mapping::new();
            for k in keys {
                let v: serde_yaml::Value = serde_yaml::to_value(&frontmatter[&k])
                    .map_err(|e| format!("serialize frontmatter key {k}: {e}"))?;
                map.insert(serde_yaml::Value::String(k), v);
            }
            let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(map))
                .map_err(|e| format!("serialize frontmatter: {e}"))?;
            out.push_str("---\n");
            out.push_str(&yaml);
            out.push_str("---\n\n");
        }
        out.push_str(&body);
        let path = dir.join("SKILL.md");
        std::fs::write(&path, out).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path.display().to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}
