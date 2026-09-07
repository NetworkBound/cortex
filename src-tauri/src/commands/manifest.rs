//! Aider-style **file-manifest mode** — `/add`, `/drop`, `/ls`.
//!
//! Aider lets you explicitly *add files to the chat* (`/add`), so their full
//! current contents are sent to the model with every message; `/drop` removes
//! them and `/ls` lists what's in the chat. This is distinct from Cortex's
//! existing auto-context (the ranked [`crate::repo_map`] gives *signatures* of
//! the most central files; `@`-mentions resolve content on demand): the manifest
//! is the user's **deliberate, persistent** working set — "these specific files
//! are what we're editing; always keep them in front of you."
//!
//! This module owns:
//!   * a **per-project manifest** persisted at
//!     `<project_root>/.cortex/manifest.json` — a JSON array of project-relative,
//!     forward-slash paths (so it round-trips across OSes and is diff-friendly);
//!   * **path confinement** — every added path is resolved against the project
//!     root and any absolute-outside / `..`-escape is refused (a manifest can
//!     only ever reference files *inside* the open project, mirroring
//!     [`super::apply_edits`]);
//!   * [`build_manifest_block`] — read the manifested files' **live** contents at
//!     send time and emit a `<files>` context block, bounded so the working set
//!     can never blow the context budget.
//!
//! The load/save, normalization, and block-building cores are pure aside from the
//! filesystem and are unit-tested on real temp dirs.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Hard cap on the total bytes of file content emitted in the `<files>` block.
/// The manifest is full file content (not a map), so it gets a generous-but-
/// bounded budget; past this we stop adding whole files and note the omission.
pub const MAX_BLOCK_BYTES: usize = 32 * 1024;

/// Per-file cap inside the block, so one large file can't crowd out the rest of
/// the working set. Clipped on a UTF-8 boundary with a trailing marker.
pub const MAX_FILE_BYTES: usize = 16 * 1024;

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("manifest.json")
}

/// Lexically collapse `.`/`..` without touching the filesystem (so confinement
/// works even for paths that don't exist yet and never follows symlinks).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a user-supplied path to a project-relative, forward-slash string,
/// confined to `root`. Accepts a relative path or an absolute path that lands
/// *inside* root; refuses empties, NULs, the root itself, and any `..` escape.
fn normalize_rel(root: &Path, input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("empty path".into());
    }
    if t.contains('\0') {
        return Err("path contains NUL".into());
    }
    let pb = PathBuf::from(t);
    let joined = if pb.is_absolute() {
        lexical_normalize(&pb)
    } else {
        lexical_normalize(&root.join(&pb))
    };
    let root_norm = lexical_normalize(root);
    let rel = joined
        .strip_prefix(&root_norm)
        .map_err(|_| format!("path escapes the project: {t}"))?;
    if rel.as_os_str().is_empty() {
        return Err("path is the project root itself".into());
    }
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok(parts.join("/"))
}

/// Load the raw manifest (project-relative paths) for a project, preserving
/// insertion order. A missing or malformed file reads as an empty manifest so a
/// project that never opted in is a true no-op.
pub fn load_manifest(project_root: &Path) -> Vec<String> {
    let path = config_path(project_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_str::<Vec<String>>(&raw) {
        Ok(v) => {
            let mut seen = std::collections::HashSet::new();
            v.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .filter(|s| seen.insert(s.clone()))
                .collect()
        }
        Err(e) => {
            tracing::warn!("manifest: bad json at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Persist the manifest, creating `.cortex/` as needed. An **empty** manifest
/// removes the file, so "nothing in the chat" has one canonical representation.
pub fn save_manifest(project_root: &Path, entries: &[String]) -> anyhow::Result<()> {
    let path = config_path(project_root);
    if entries.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(entries)?;
    std::fs::write(&path, body)?;
    Ok(())
}

/// One manifest entry surfaced to the UI, with live existence/size so `/ls` can
/// flag a file that was added then deleted/moved on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    /// Project-relative, forward-slash path.
    pub path: String,
    /// Whether the file still exists on disk right now.
    pub exists: bool,
    /// Size in bytes when it exists.
    pub size: Option<u64>,
}

fn to_entries(root: &Path, paths: &[String]) -> Vec<ManifestEntry> {
    paths
        .iter()
        .map(|p| {
            let abs = root.join(p);
            let meta = std::fs::metadata(&abs).ok();
            ManifestEntry {
                path: p.clone(),
                exists: meta.as_ref().map(|m| m.is_file()).unwrap_or(false),
                size: meta.as_ref().map(|m| m.len()),
            }
        })
        .collect()
}

/// Why an added path was skipped (surfaced to the user verbatim).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedPath {
    pub path: String,
    pub reason: String,
}

/// Result of an `/add` — what newly landed, what was already there, what was
/// refused, and the resulting manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub added: Vec<String>,
    pub already: Vec<String>,
    pub skipped: Vec<SkippedPath>,
    pub manifest: Vec<ManifestEntry>,
}

/// Add `inputs` to the project's manifest. Each is confined to the project root
/// and must be an existing file; valid ones are appended (dedup, order-stable),
/// invalid ones reported in `skipped`. Persists only when something changed.
pub fn add_paths(root: &Path, inputs: &[String]) -> Result<AddResult, String> {
    let mut manifest = load_manifest(root);
    let mut added = Vec::new();
    let mut already = Vec::new();
    let mut skipped = Vec::new();

    for input in inputs {
        let rel = match normalize_rel(root, input) {
            Ok(r) => r,
            Err(e) => {
                skipped.push(SkippedPath {
                    path: input.trim().to_string(),
                    reason: e,
                });
                continue;
            }
        };
        let abs = root.join(&rel);
        if !abs.is_file() {
            skipped.push(SkippedPath {
                path: rel,
                reason: if abs.is_dir() {
                    "is a directory, not a file".into()
                } else {
                    "no such file".into()
                },
            });
            continue;
        }
        if manifest.contains(&rel) {
            already.push(rel);
            continue;
        }
        manifest.push(rel.clone());
        added.push(rel);
    }

    if !added.is_empty() {
        save_manifest(root, &manifest).map_err(|e| format!("save failed: {e}"))?;
    }
    Ok(AddResult {
        added,
        already,
        skipped,
        manifest: to_entries(root, &manifest),
    })
}

/// Drop `inputs` from the manifest; an **empty** `inputs` clears the whole
/// manifest (aider's bare `/drop`). Returns the resulting entries. Matching is
/// by normalized relative path, so the same path adds and drops consistently
/// regardless of how it was typed.
pub fn drop_paths(root: &Path, inputs: &[String]) -> Result<Vec<ManifestEntry>, String> {
    let mut manifest = load_manifest(root);
    if inputs.is_empty() {
        manifest.clear();
    } else {
        let targets: std::collections::HashSet<String> = inputs
            .iter()
            .filter_map(|i| normalize_rel(root, i).ok())
            .collect();
        manifest.retain(|p| !targets.contains(p));
    }
    save_manifest(root, &manifest).map_err(|e| format!("save failed: {e}"))?;
    Ok(to_entries(root, &manifest))
}

/// Keep the **head** of `s` within `cap` bytes (UTF-8 boundary), returning
/// whether anything was dropped.
fn clip_head(s: &str, cap: usize) -> (&str, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    let mut cut = cap;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (&s[..cut], true)
}

/// Map a file extension to a fenced-code language hint (best-effort; unknown
/// extensions fall back to the extension itself, which is still useful).
fn lang_for(rel: &str) -> &str {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "sh" | "bash" | "zsh" => "bash",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        other => other,
    }
}

/// Build the `<files>` context block from the manifest's **live** contents, or
/// `None` when the manifest is empty. Each file is read fresh (so edits are
/// reflected), per-file head-clipped to [`MAX_FILE_BYTES`], and the whole block
/// bounded by [`MAX_BLOCK_BYTES`]; a missing file is noted rather than dropped
/// silently, and files that overflow the budget are summarized at the end.
pub fn build_manifest_block(root: &Path) -> Option<String> {
    let manifest = load_manifest(root);
    if manifest.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut used = 0usize;
    let mut omitted: Vec<String> = Vec::new();

    for rel in &manifest {
        let abs = root.join(rel);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => {
                body.push_str(&format!("### {rel} (missing — was removed on disk)\n\n"));
                continue;
            }
        };
        // Budget check before committing this file's content.
        if used >= MAX_BLOCK_BYTES {
            omitted.push(rel.clone());
            continue;
        }
        let (clipped, was_clipped) = clip_head(&content, MAX_FILE_BYTES);
        let lang = lang_for(rel);
        let mut section = format!("### {rel}\n```{lang}\n{clipped}");
        if !clipped.ends_with('\n') {
            section.push('\n');
        }
        if was_clipped {
            section.push_str("… (truncated)\n");
        }
        section.push_str("```\n\n");
        used += section.len();
        body.push_str(&section);
    }

    if !omitted.is_empty() {
        body.push_str(&format!(
            "_(Omitted to stay within the context budget: {})_\n",
            omitted.join(", ")
        ));
    }

    if body.trim().is_empty() {
        return None;
    }
    Some(format!(
        "<files>\nThe user has explicitly added these files to the chat. Their full current contents are below — treat them as the working set and refer to them directly.\n\n{body}</files>\n\n"
    ))
}

// ---- Tauri commands -----------------------------------------------------

/// List the files currently in the chat (the manifest), with live existence.
#[tauri::command]
pub async fn get_manifest(project_root: String) -> Result<Vec<ManifestEntry>, String> {
    let root = PathBuf::from(&project_root);
    Ok(to_entries(&root, &load_manifest(&root)))
}

/// Add one or more files to the manifest (`/add`).
#[tauri::command]
pub async fn add_to_manifest(
    project_root: String,
    paths: Vec<String>,
) -> Result<AddResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    add_paths(&root, &paths)
}

/// Drop files from the manifest, or clear it entirely when `paths` is empty
/// (`/drop`).
#[tauri::command]
pub async fn drop_from_manifest(
    project_root: String,
    paths: Vec<String>,
) -> Result<Vec<ManifestEntry>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    drop_paths(&root, &paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(root: &Path, rel: &str, body: &str) {
        let abs = root.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(abs, body).unwrap();
    }

    #[test]
    fn add_then_load_round_trips_and_dedups() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "src/main.rs", "fn main() {}");
        let r = add_paths(td.path(), &["src/main.rs".into()]).unwrap();
        assert_eq!(r.added, vec!["src/main.rs"]);
        assert_eq!(load_manifest(td.path()), vec!["src/main.rs"]);
        // Adding again reports `already`, doesn't duplicate.
        let r2 = add_paths(td.path(), &["src/main.rs".into()]).unwrap();
        assert!(r2.added.is_empty());
        assert_eq!(r2.already, vec!["src/main.rs"]);
        assert_eq!(load_manifest(td.path()), vec!["src/main.rs"]);
    }

    #[test]
    fn add_normalizes_dot_and_absolute_paths() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "a/b.txt", "hi");
        // `./a/b.txt` and an absolute path to the same file normalize identically.
        add_paths(td.path(), &["./a/b.txt".into()]).unwrap();
        let abs = td.path().join("a/b.txt").display().to_string();
        let r = add_paths(td.path(), &[abs]).unwrap();
        assert!(r.added.is_empty(), "absolute dup should not re-add");
        assert_eq!(r.already, vec!["a/b.txt"]);
        assert_eq!(load_manifest(td.path()), vec!["a/b.txt"]);
    }

    #[test]
    fn add_rejects_escape_and_missing() {
        let td = TempDir::new().unwrap();
        let r = add_paths(
            td.path(),
            &["../../etc/passwd".into(), "nope.rs".into()],
        )
        .unwrap();
        assert!(r.added.is_empty());
        assert_eq!(r.skipped.len(), 2);
        assert!(r.skipped[0].reason.contains("escapes"));
        assert!(r.skipped[1].reason.contains("no such file"));
        assert!(load_manifest(td.path()).is_empty());
    }

    #[test]
    fn add_rejects_directory() {
        let td = TempDir::new().unwrap();
        std::fs::create_dir(td.path().join("src")).unwrap();
        let r = add_paths(td.path(), &["src".into()]).unwrap();
        assert!(r.added.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("directory"));
    }

    #[test]
    fn drop_specific_and_drop_all() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "a.rs", "");
        touch(td.path(), "b.rs", "");
        add_paths(td.path(), &["a.rs".into(), "b.rs".into()]).unwrap();
        // Drop one by a non-canonical spelling (`./a.rs`) — still matches.
        let left = drop_paths(td.path(), &["./a.rs".into()]).unwrap();
        assert_eq!(left.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), vec!["b.rs"]);
        // Bare drop clears everything and removes the file.
        let empty = drop_paths(td.path(), &[]).unwrap();
        assert!(empty.is_empty());
        assert!(!config_path(td.path()).exists(), "empty manifest removes the file");
    }

    #[test]
    fn load_missing_and_malformed_is_empty() {
        let td = TempDir::new().unwrap();
        assert!(load_manifest(td.path()).is_empty());
        std::fs::create_dir_all(td.path().join(".cortex")).unwrap();
        std::fs::write(config_path(td.path()), "{ not json").unwrap();
        assert!(load_manifest(td.path()).is_empty());
    }

    #[test]
    fn entries_flag_a_deleted_file() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "gone.rs", "x");
        add_paths(td.path(), &["gone.rs".into()]).unwrap();
        std::fs::remove_file(td.path().join("gone.rs")).unwrap();
        let entries = to_entries(td.path(), &load_manifest(td.path()));
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].exists);
        assert!(entries[0].size.is_none());
    }

    #[test]
    fn block_is_none_when_empty() {
        let td = TempDir::new().unwrap();
        assert!(build_manifest_block(td.path()).is_none());
    }

    #[test]
    fn block_embeds_live_content_with_lang_fence() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "src/lib.rs", "pub fn answer() -> u8 { 42 }\n");
        add_paths(td.path(), &["src/lib.rs".into()]).unwrap();
        let block = build_manifest_block(td.path()).unwrap();
        assert!(block.starts_with("<files>"));
        assert!(block.contains("### src/lib.rs"));
        assert!(block.contains("```rust"));
        assert!(block.contains("pub fn answer() -> u8 { 42 }"));
        assert!(block.trim_end().ends_with("</files>"));
        // Reflects live edits.
        touch(td.path(), "src/lib.rs", "pub fn answer() -> u8 { 7 }\n");
        let block2 = build_manifest_block(td.path()).unwrap();
        assert!(block2.contains("{ 7 }") && !block2.contains("{ 42 }"));
    }

    #[test]
    fn block_notes_a_missing_file() {
        let td = TempDir::new().unwrap();
        touch(td.path(), "x.rs", "fn x() {}");
        add_paths(td.path(), &["x.rs".into()]).unwrap();
        std::fs::remove_file(td.path().join("x.rs")).unwrap();
        let block = build_manifest_block(td.path()).unwrap();
        assert!(block.contains("### x.rs (missing"));
    }

    #[test]
    fn block_clips_a_huge_file_to_the_per_file_cap() {
        let td = TempDir::new().unwrap();
        let big = "x".repeat(MAX_FILE_BYTES * 2);
        touch(td.path(), "big.txt", &big);
        add_paths(td.path(), &["big.txt".into()]).unwrap();
        let block = build_manifest_block(td.path()).unwrap();
        assert!(block.contains("… (truncated)"));
        assert!(block.len() < MAX_FILE_BYTES * 2);
    }
}
