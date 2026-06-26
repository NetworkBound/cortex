//! Continue.dev-style **model roles** — a per-project default model per logical
//! role.
//!
//! Continue lets you assign a specific model to each *role* (chat, edit/apply,
//! autocomplete, …) so the right model handles the right job without re-picking
//! every turn. Cortex already dispatches three logical roles to potentially
//! different models — the **chat** model (Auto-selected or picked in the
//! composer), and the Aider-style architect split's **planner** and **editor**
//! models — but the only defaults were hardcoded constants
//! (`DEFAULT_PLANNER_MODEL`/`DEFAULT_EDITOR_MODEL`) plus the per-turn pick. This
//! module adds a persisted, per-project mapping `role → default model id`,
//! consulted as a **low-precedence default**: an explicit per-turn pick or an
//! `/architect *_model=` override still wins, so a project can pin e.g. a local
//! Ollama model for chat and Opus for planning without re-picking every turn.
//!
//! Persisted at `<project_root>/.cortex/model-roles.toml`:
//! ```toml
//! chat = "ollama:llama3.2:1b"
//! planner = "claude-opus-4-8"
//! editor = "claude-sonnet-4-6"
//! ```
//! A missing file, malformed TOML, or a blank field all read as *unset* for that
//! role, so a project that never opts in behaves exactly as before. The values
//! are stored verbatim and canonicalized through the alias catalog at the point
//! of use (so `opus` resolves to `claude-opus-4-8` just like a typed pick).
//!
//! The load/write core is pure aside from the filesystem and is unit-tested
//! end-to-end on real temp dirs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk + wire schema for `.cortex/model-roles.toml`. Every role is optional;
/// an absent or blank field means "no configured default for this role".
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelRoles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
}

impl ModelRoles {
    /// Collapse blank / whitespace-only fields to `None` so a UI that submits an
    /// empty string for a role clears it (single canonical "unset" = `None`).
    fn normalized(self) -> Self {
        let clean = |o: Option<String>| {
            o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        Self {
            chat: clean(self.chat),
            planner: clean(self.planner),
            editor: clean(self.editor),
        }
    }

    /// True when no role is assigned — the canonical "off" state, persisted as
    /// the *absence* of the file.
    fn is_empty(&self) -> bool {
        self.chat.is_none() && self.planner.is_none() && self.editor.is_none()
    }
}

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("model-roles.toml")
}

/// Load the per-project model-role map. A missing file or malformed TOML yields
/// an all-`None` map (feature off); blank fields normalize to `None`. Never
/// errors — a project that never opts in must behave exactly as before.
pub fn load_model_roles(project_root: &Path) -> ModelRoles {
    let path = config_path(project_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return ModelRoles::default(),
    };
    match toml::from_str::<ModelRoles>(&raw) {
        Ok(v) => v.normalized(),
        Err(e) => {
            tracing::warn!("model_roles: bad toml at {}: {e}", path.display());
            ModelRoles::default()
        }
    }
}

/// Persist (or clear) the model-role map, creating `.cortex/` if needed. An
/// all-empty map **removes** the file so the "off" state has a single
/// representation (absence) that [`load_model_roles`] already maps to empty.
pub fn write_model_roles(project_root: &Path, roles: &ModelRoles) -> anyhow::Result<()> {
    let path = config_path(project_root);
    let roles = roles.clone().normalized();
    if roles.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string(&roles)?)?;
        Ok(())
    }
}

/// Read the configured model-role map for a project. A blank/absent root or
/// missing config yields an empty map — never an error.
#[tauri::command]
pub async fn get_model_roles(project_root: String) -> Result<ModelRoles, String> {
    if project_root.trim().is_empty() {
        return Ok(ModelRoles::default());
    }
    Ok(load_model_roles(Path::new(&project_root)))
}

/// Persist the model-role map for a project and return it as stored (blank
/// fields normalized away). An all-empty map clears the config file.
#[tauri::command]
pub async fn set_model_roles(
    project_root: String,
    roles: ModelRoles,
) -> Result<ModelRoles, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = Path::new(&project_root);
    write_model_roles(root, &roles).map_err(|e| e.to_string())?;
    Ok(load_model_roles(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_config_is_empty() {
        let d = td();
        let r = load_model_roles(d.path());
        assert_eq!(r, ModelRoles::default());
        assert!(r.is_empty());
    }

    #[test]
    fn write_then_load_round_trips() {
        let d = td();
        let roles = ModelRoles {
            chat: Some("ollama:llama3.2:1b".into()),
            planner: Some("claude-opus-4-8".into()),
            editor: Some("claude-sonnet-4-6".into()),
        };
        write_model_roles(d.path(), &roles).unwrap();
        // The file lives under .cortex/.
        assert!(d.path().join(".cortex").join("model-roles.toml").exists());
        assert_eq!(load_model_roles(d.path()), roles);
    }

    #[test]
    fn partial_assignment_round_trips() {
        let d = td();
        let roles = ModelRoles {
            chat: None,
            planner: Some("claude-opus-4-8".into()),
            editor: None,
        };
        write_model_roles(d.path(), &roles).unwrap();
        let loaded = load_model_roles(d.path());
        assert_eq!(loaded.planner.as_deref(), Some("claude-opus-4-8"));
        assert!(loaded.chat.is_none());
        assert!(loaded.editor.is_none());
    }

    #[test]
    fn blank_fields_normalize_to_none() {
        let d = td();
        let roles = ModelRoles {
            chat: Some("  ".into()),
            planner: Some("\t".into()),
            editor: Some("  claude-opus-4-8  ".into()),
        };
        write_model_roles(d.path(), &roles).unwrap();
        let loaded = load_model_roles(d.path());
        assert!(loaded.chat.is_none());
        assert!(loaded.planner.is_none());
        // Whitespace trimmed on the real value.
        assert_eq!(loaded.editor.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn empty_map_clears_the_file() {
        let d = td();
        let roles = ModelRoles {
            chat: Some("claude-opus-4-8".into()),
            ..Default::default()
        };
        write_model_roles(d.path(), &roles).unwrap();
        assert!(d.path().join(".cortex").join("model-roles.toml").exists());
        // Writing an all-empty map removes the file (canonical "off").
        write_model_roles(d.path(), &ModelRoles::default()).unwrap();
        assert!(!d.path().join(".cortex").join("model-roles.toml").exists());
        // Clearing an already-absent config is a no-op, not an error.
        write_model_roles(d.path(), &ModelRoles::default()).unwrap();
        assert_eq!(load_model_roles(d.path()), ModelRoles::default());
    }

    #[test]
    fn malformed_toml_reads_as_empty() {
        let d = td();
        std::fs::create_dir_all(d.path().join(".cortex")).unwrap();
        std::fs::write(
            d.path().join(".cortex").join("model-roles.toml"),
            "chat = [not valid",
        )
        .unwrap();
        assert_eq!(load_model_roles(d.path()), ModelRoles::default());
    }

    #[tokio::test]
    async fn command_round_trip_and_blank_root_guard() {
        let d = td();
        let root = d.path().display().to_string();
        // Blank root: get → empty, set → error (never writes outside a project).
        assert_eq!(get_model_roles(String::new()).await.unwrap(), ModelRoles::default());
        assert!(set_model_roles("   ".into(), ModelRoles::default()).await.is_err());
        // Set via the command, then read it back via the command.
        let roles = ModelRoles {
            chat: Some("ollama:llama3.2:1b".into()),
            planner: Some("  opus  ".into()), // trimmed (not canonicalized here)
            editor: None,
        };
        let stored = set_model_roles(root.clone(), roles).await.unwrap();
        assert_eq!(stored.chat.as_deref(), Some("ollama:llama3.2:1b"));
        assert_eq!(stored.planner.as_deref(), Some("opus"));
        assert!(stored.editor.is_none());
        assert_eq!(get_model_roles(root.clone()).await.unwrap(), stored);
        // Clearing via an empty map removes the config.
        set_model_roles(root.clone(), ModelRoles::default()).await.unwrap();
        assert_eq!(get_model_roles(root).await.unwrap(), ModelRoles::default());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // A forward-compatible config with an extra key still loads the known
        // roles (serde ignores unknown fields by default).
        let d = td();
        std::fs::create_dir_all(d.path().join(".cortex")).unwrap();
        std::fs::write(
            d.path().join(".cortex").join("model-roles.toml"),
            "chat = \"claude-opus-4-8\"\nautocomplete = \"future-model\"\n",
        )
        .unwrap();
        let loaded = load_model_roles(d.path());
        assert_eq!(loaded.chat.as_deref(), Some("claude-opus-4-8"));
    }
}
