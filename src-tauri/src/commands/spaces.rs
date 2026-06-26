//! Spaces — scoped subsets of a project (e.g. frontend/backend/docs) with
//! their own glob-defined view over the file tree. Backed by
//! `<project_root>/.cortex/spaces.yaml`. Missing file == no spaces (not an
//! error), so a fresh project just shows an empty list.
//!
//! YAML shape:
//! ```yaml
//! spaces:
//!   - name: frontend
//!     description: React + TS components
//!     includes:
//!       - src/**/*.tsx
//!     excludes:
//!       - src-tauri/**
//! ```
//!
//! Glob semantics use `globset` (same crate as `.cortexignore`), so users can
//! reuse intuition. Excludes win over includes — a path that matches both is
//! treated as excluded.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::projects::list_files;

/// Single space definition as stored in `.cortex/spaces.yaml`. Default
/// derivations let the frontend send partial structs (e.g. no excludes).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Space {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub includes: Vec<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// Top-level YAML document. Wrapping the list in a `spaces:` key keeps room
/// for future config (e.g. a default-space pointer) without breaking files.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SpacesDoc {
    #[serde(default)]
    spaces: Vec<Space>,
}

fn spaces_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("spaces.yaml")
}

/// Load + parse the yaml. Empty/missing file => empty doc so callers don't
/// have to special-case first-run state. Parse errors bubble up so users see
/// "your spaces.yaml is broken" instead of silently dropping their config.
fn load_doc(project_root: &Path) -> Result<SpacesDoc, String> {
    let path = spaces_path(project_root);
    if !path.exists() {
        return Ok(SpacesDoc::default());
    }
    let body = fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if body.trim().is_empty() {
        return Ok(SpacesDoc::default());
    }
    serde_yaml::from_str(&body).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn save_doc(project_root: &Path, doc: &SpacesDoc) -> Result<(), String> {
    let path = spaces_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let body = serde_yaml::to_string(doc).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Compile a list of glob patterns into a GlobSet. Invalid patterns are
/// silently dropped (logged) so a single typo doesn't poison the whole space.
fn compile_globs(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            continue;
        }
        match Glob::new(trimmed) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                tracing::warn!("space: bad glob '{trimmed}': {e}");
            }
        }
    }
    builder
        .build()
        .unwrap_or_else(|_| GlobSetBuilder::new().build().expect("empty ok"))
}

/// Walks the project (via `list_files`) and returns paths that match a
/// space's includes (minus its excludes). Paths are emitted as relative
/// strings so the UI can render them cleanly.
fn matching_files(project_root: &Path, space: &Space, limit: usize) -> Vec<String> {
    let includes = compile_globs(&space.includes);
    let excludes = compile_globs(&space.excludes);
    // Walk a large slice (5k) of the project file tree — list_files already
    // honours `.cortexignore` and the builtin denylist, so we never see
    // `.git/`, `node_modules/`, etc. here.
    let entries = list_files(project_root, 5000);
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let rel = entry
            .path
            .strip_prefix(project_root)
            .unwrap_or(&entry.path)
            .to_path_buf();
        if !includes.is_match(&rel) {
            continue;
        }
        if excludes.is_match(&rel) {
            continue;
        }
        out.push(rel.display().to_string());
        if out.len() >= limit {
            break;
        }
    }
    out.sort();
    out
}

#[tauri::command]
pub async fn list_spaces(project_root: String) -> Result<Vec<Space>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(load_doc(&root)?.spaces)
}

#[tauri::command]
pub async fn save_space(project_root: String, space: Space) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let name = space.name.trim();
    if name.is_empty() {
        return Err("space name is required".into());
    }
    let mut doc = load_doc(&root)?;
    // Upsert by name (case-insensitive) so users can rename description /
    // globs without seeing the same entry twice.
    let mut found = false;
    for existing in doc.spaces.iter_mut() {
        if existing.name.eq_ignore_ascii_case(name) {
            *existing = space.clone();
            found = true;
            break;
        }
    }
    if !found {
        doc.spaces.push(space);
    }
    save_doc(&root, &doc)
}

#[tauri::command]
pub async fn delete_space(project_root: String, name: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let mut doc = load_doc(&root)?;
    let before = doc.spaces.len();
    doc.spaces
        .retain(|s| !s.name.eq_ignore_ascii_case(name.trim()));
    if doc.spaces.len() == before {
        // Not an error — idempotent delete keeps the UI dumb.
        return Ok(());
    }
    save_doc(&root, &doc)
}

#[tauri::command]
pub async fn space_files(
    project_root: String,
    name: String,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let doc = load_doc(&root)?;
    let Some(space) = doc
        .spaces
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name.trim()))
    else {
        return Err(format!("no space named '{name}'"));
    };
    Ok(matching_files(&root, &space, limit.unwrap_or(1000)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_yaml_returns_empty() {
        let td = TempDir::new().unwrap();
        let res = tauri::async_runtime::block_on(list_spaces(
            td.path().display().to_string(),
        ))
        .unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn save_then_list_roundtrips() {
        let td = TempDir::new().unwrap();
        let root = td.path().display().to_string();
        let space = Space {
            name: "frontend".into(),
            description: "ui".into(),
            includes: vec!["src/**/*.tsx".into()],
            excludes: vec![],
        };
        tauri::async_runtime::block_on(save_space(root.clone(), space)).unwrap();
        let listed =
            tauri::async_runtime::block_on(list_spaces(root.clone())).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "frontend");
        // Upsert: same name overwrites.
        let updated = Space {
            name: "frontend".into(),
            description: "ui v2".into(),
            includes: vec!["src/**/*.ts".into()],
            excludes: vec![],
        };
        tauri::async_runtime::block_on(save_space(root.clone(), updated)).unwrap();
        let listed2 =
            tauri::async_runtime::block_on(list_spaces(root.clone())).unwrap();
        assert_eq!(listed2.len(), 1);
        assert_eq!(listed2[0].description, "ui v2");
    }

    #[test]
    fn delete_is_idempotent() {
        let td = TempDir::new().unwrap();
        let root = td.path().display().to_string();
        // Delete on missing file is OK.
        tauri::async_runtime::block_on(delete_space(root.clone(), "x".into())).unwrap();
        tauri::async_runtime::block_on(save_space(
            root.clone(),
            Space {
                name: "x".into(),
                ..Default::default()
            },
        ))
        .unwrap();
        tauri::async_runtime::block_on(delete_space(root.clone(), "x".into())).unwrap();
        let listed =
            tauri::async_runtime::block_on(list_spaces(root.clone())).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn space_files_filters_by_globs() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("src-tauri")).unwrap();
        fs::write(root.join("src").join("App.tsx"), "x").unwrap();
        fs::write(root.join("src").join("App.css"), "x").unwrap();
        fs::write(root.join("src-tauri").join("main.rs"), "x").unwrap();
        let root_s = root.display().to_string();
        tauri::async_runtime::block_on(save_space(
            root_s.clone(),
            Space {
                name: "fe".into(),
                description: "".into(),
                includes: vec!["src/**/*.tsx".into()],
                excludes: vec![],
            },
        ))
        .unwrap();
        let files = tauri::async_runtime::block_on(space_files(
            root_s.clone(),
            "fe".into(),
            None,
        ))
        .unwrap();
        assert!(files.iter().any(|p| p.ends_with("App.tsx")));
        assert!(!files.iter().any(|p| p.ends_with("App.css")));
        assert!(!files.iter().any(|p| p.ends_with("main.rs")));
    }
}
