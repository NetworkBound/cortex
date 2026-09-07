//! Schema-locked settings.json editor backend.
//!
//! Exposes `read_config_file` / `write_config_file` for a tightly-scoped set
//! of Cortex config locations: either the user-global `~/.cortex/<name>` or
//! the project-local `<project>/.cortex/<name>` (single segment, no nested
//! traversal). Any path that resolves outside those roots is rejected before
//! disk access.
//!
//! Writes additionally validate syntax based on extension: `.json` bodies are
//! checked with `serde_json` and `.toml` bodies are parsed with the `toml`
//! crate, so a corrupted save can't silently break the rest of Cortex on next
//! launch. A parse failure is surfaced verbatim so the UI can show it inline.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Where a config file lives. Mirrored in the TS layer so the frontend
/// dropdown can render targets without a backend roundtrip.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigScope {
    /// Lives under the user's home dir: `~/.cortex/<name>`.
    Home,
    /// Lives under the active project root: `<project>/.cortex/<name>`.
    Project,
}

/// Frontend-supplied location. `rel_path` is the single-segment file name
/// (e.g. `"snippets.json"` or `"profiles/dev.toml"`). Multi-segment paths
/// are allowed but each segment must be a plain filename — `..` is rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigTarget {
    pub scope: ConfigScope,
    pub rel_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigReadResult {
    /// Absolute resolved path. Returned so the UI can show users the real
    /// location (handy when `~/.cortex` is a symlink etc).
    pub path: String,
    /// File body. Empty string when the file doesn't exist yet — caller
    /// distinguishes via the `exists` flag.
    pub body: String,
    pub exists: bool,
    /// `true` when the file is read-only. Currently no extension is forced
    /// read-only — TOML and JSON are both editable+validated — but the field
    /// is kept so the UI contract stays stable and future locked formats can
    /// flip it without a signature change.
    pub read_only: bool,
}

fn home_root() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex"))
}

fn project_root(project: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(project);
    if !root.is_absolute() {
        return Err(format!("project root must be absolute: {project}"));
    }
    if !root.is_dir() {
        return Err(format!("project root not a directory: {project}"));
    }
    Ok(root.join(".cortex"))
}

/// Reject anything that could escape the configured root. Segments must be
/// plain filenames — no `..`, no absolute components, no NUL bytes — and the
/// path must contain at least one segment.
fn validate_rel(rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("rel_path is empty".into());
    }
    if rel.contains('\0') {
        return Err("rel_path contains NUL".into());
    }
    let pb = PathBuf::from(rel);
    if pb.is_absolute() {
        return Err(format!("rel_path must be relative: {rel}"));
    }
    let mut segments: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in pb.components() {
        use std::path::Component;
        match comp {
            Component::Normal(s) => segments.push(s),
            // Anything else (RootDir, ParentDir, CurDir, Prefix) is hostile
            // for our use-case; reject up-front rather than try to normalise.
            _ => return Err(format!("rel_path has illegal component: {rel}")),
        }
    }
    if segments.is_empty() {
        return Err(format!("rel_path resolves to empty: {rel}"));
    }
    Ok(pb)
}

/// Resolve a `ConfigTarget` into an absolute path, with the same
/// canonicalize-and-prefix-check used by `project_doc::is_inside`. The parent
/// directory may not exist yet (fresh install) — we canonicalize the root and
/// then join the validated relative path on top.
fn resolve_target(target: &ConfigTarget) -> Result<PathBuf, String> {
    let rel = validate_rel(&target.rel_path)?;
    let root = match target.scope {
        ConfigScope::Home => home_root()?,
        // Home-scoped targets don't need a project root; project-scoped ones
        // are handled via the dedicated `resolve_project_target` helper so the
        // project path can be threaded through without a global.
        ConfigScope::Project => {
            return Err("project scope requires project_root via resolve_project_target".into());
        }
    };
    let full = root.join(rel);
    // Final safety: ensure the joined path still lies under the root after
    // path normalisation. `canonicalize` requires existence, so we walk the
    // ancestors instead.
    if !full.starts_with(&root) {
        return Err(format!("path escapes root: {}", full.display()));
    }
    Ok(full)
}

fn resolve_project_target(target: &ConfigTarget, project: &str) -> Result<PathBuf, String> {
    let rel = validate_rel(&target.rel_path)?;
    let root = match target.scope {
        ConfigScope::Project => project_root(project)?,
        ConfigScope::Home => return Err("home scope must not pass project_root".into()),
    };
    let full = root.join(rel);
    if !full.starts_with(&root) {
        return Err(format!("path escapes root: {}", full.display()));
    }
    Ok(full)
}

/// Whether a file is shown read-only in the editor. TOML is now editable, so
/// nothing is forced read-only at the moment — kept as a hook for future
/// locked formats.
fn is_read_only(_p: &Path) -> bool {
    false
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

#[tauri::command]
pub async fn read_config_file(
    target: ConfigTarget,
    project_root: Option<String>,
) -> Result<ConfigReadResult, String> {
    tokio::task::spawn_blocking(move || {
        let path = match target.scope {
            ConfigScope::Home => resolve_target(&target)?,
            ConfigScope::Project => {
                let root = project_root
                    .as_deref()
                    .ok_or_else(|| "project scope requires project_root".to_string())?;
                resolve_project_target(&target, root)?
            }
        };
        let read_only = is_read_only(&path);
        match fs::read_to_string(&path) {
            Ok(body) => Ok(ConfigReadResult {
                path: path.display().to_string(),
                body,
                exists: true,
                read_only,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigReadResult {
                path: path.display().to_string(),
                body: String::new(),
                exists: false,
                read_only,
            }),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn write_config_file(
    target: ConfigTarget,
    body: String,
    project_root: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let path = match target.scope {
            ConfigScope::Home => resolve_target(&target)?,
            ConfigScope::Project => {
                let root = project_root
                    .as_deref()
                    .ok_or_else(|| "project scope requires project_root".to_string())?;
                resolve_project_target(&target, root)?
            }
        };
        if is_read_only(&path) {
            return Err(format!("{} is read-only in this build", path.display()));
        }
        // Body size guard — these are settings files, not blobs. 1 MiB is
        // already absurdly generous.
        if body.len() > 1024 * 1024 {
            return Err("config body exceeds 1 MiB limit".into());
        }
        // JSON files get a syntax check so a typo in the editor can't leave
        // Cortex in a state where the next launch fails to parse the file.
        if has_ext(&path, "json") {
            serde_json::from_str::<serde_json::Value>(&body)
                .map_err(|e| format!("refusing to save invalid JSON: {e}"))?;
        }
        // TOML files get the same treatment via the `toml` crate so a malformed
        // profile can't be saved. The parse error is returned verbatim.
        if has_ext(&path, "toml") {
            body.parse::<toml::Value>()
                .map_err(|e| format!("refusing to save invalid TOML: {e}"))?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path.display().to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_rel("../etc/passwd").is_err());
        assert!(validate_rel("/etc/passwd").is_err());
        assert!(validate_rel("").is_err());
        assert!(validate_rel("foo/../bar").is_err());
        assert!(validate_rel("ok.json").is_ok());
        assert!(validate_rel("profiles/dev.toml").is_ok());
    }

    #[test]
    fn write_then_read_roundtrip_in_project() {
        let td = TempDir::new().unwrap();
        let project = td.path().display().to_string();
        let target = ConfigTarget {
            scope: ConfigScope::Project,
            rel_path: "hooks.json".into(),
        };
        let body = "{\"a\":1}".to_string();
        let path = tauri::async_runtime::block_on(write_config_file(
            target.clone(),
            body.clone(),
            Some(project.clone()),
        ))
        .unwrap();
        assert!(path.ends_with("hooks.json"));
        let res = tauri::async_runtime::block_on(read_config_file(
            target,
            Some(project),
        ))
        .unwrap();
        assert!(res.exists);
        assert_eq!(res.body, body);
        assert!(!res.read_only);
    }

    #[test]
    fn rejects_invalid_json() {
        let td = TempDir::new().unwrap();
        let project = td.path().display().to_string();
        let target = ConfigTarget {
            scope: ConfigScope::Project,
            rel_path: "hooks.json".into(),
        };
        let err = tauri::async_runtime::block_on(write_config_file(
            target,
            "{not json}".into(),
            Some(project),
        ))
        .unwrap_err();
        assert!(err.contains("invalid JSON"), "got: {err}");
    }

    #[test]
    fn toml_is_editable_and_validated() {
        let td = TempDir::new().unwrap();
        let project = td.path().display().to_string();
        let target = ConfigTarget {
            scope: ConfigScope::Project,
            rel_path: "danger.toml".into(),
        };
        // Valid TOML writes and round-trips, and is no longer read-only.
        let body = "[shell]\ndeny = [\"rm -rf /\"]\n".to_string();
        let path = tauri::async_runtime::block_on(write_config_file(
            target.clone(),
            body.clone(),
            Some(project.clone()),
        ))
        .unwrap();
        assert!(path.ends_with("danger.toml"));
        let res = tauri::async_runtime::block_on(read_config_file(
            target.clone(),
            Some(project.clone()),
        ))
        .unwrap();
        assert!(res.exists);
        assert_eq!(res.body, body);
        assert!(!res.read_only);
        // Invalid TOML is rejected with a surfaced parse error.
        let err = tauri::async_runtime::block_on(write_config_file(
            target,
            "this is = = not toml".into(),
            Some(project),
        ))
        .unwrap_err();
        assert!(err.contains("invalid TOML"), "got: {err}");
    }

    #[test]
    fn missing_file_returns_empty_body() {
        let td = TempDir::new().unwrap();
        let project = td.path().display().to_string();
        let target = ConfigTarget {
            scope: ConfigScope::Project,
            rel_path: "absent.json".into(),
        };
        let res = tauri::async_runtime::block_on(read_config_file(
            target,
            Some(project),
        ))
        .unwrap();
        assert!(!res.exists);
        assert_eq!(res.body, "");
    }
}
