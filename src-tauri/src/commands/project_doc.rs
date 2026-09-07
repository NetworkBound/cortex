//! AGENTS.md hierarchical project memory (Codex / Cursor / Zed convention).
//!
//! At run-time we merge `AGENTS.md` files from five well-known locations
//! into a single system-prompt prefix. Cortex-specific overrides take
//! precedence, falling back to Codex's home-directory convention so users
//! who already maintain `~/.codex/AGENTS.md` get it for free.
//!
//! Search order (later entries override earlier ones in the merged view —
//! we keep the originals around so the UI can show each segment separately):
//!
//!  1. `~/.cortex/AGENTS.md`          — `global`  (process-wide user prefs)
//!  2. `~/.codex/AGENTS.md`           — `codex`   (cross-tool compatibility)
//!  3. `<project_root>/AGENTS.md`     — `project` (repo-wide rules)
//!  4. `<project_root>/.cortex/AGENTS.md` — `cortex` (cortex-only override)
//!  5. `<cwd>/AGENTS.md`              — `cwd`     (subdirectory scope; only
//!                                                 surfaced when `cwd` is
//!                                                 *inside* and not equal to
//!                                                 `project_root`)
//!
//! Each body is capped at 16 KiB so a runaway document can't blow the context
//! budget. The cap is enforced char-wise (we slice safely on char boundaries).

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Per-file cap. 16 KiB matches the rough upper bound Codex documents for
/// reasonable per-scope instructions; anything larger usually belongs in a
/// linked runbook.
const MAX_BODY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct AgentsDocSegment {
    pub path: PathBuf,
    pub body: String,
    /// One of `global` | `codex` | `project` | `cortex` | `cwd`.
    pub scope: &'static str,
}

fn read_capped(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    if body.as_bytes().len() <= MAX_BODY_BYTES {
        return Some(body);
    }
    // Trim on a char boundary to keep the string UTF-8 valid. We approximate
    // the byte budget by taking chars until we'd exceed it.
    let mut out = String::with_capacity(MAX_BODY_BYTES);
    for ch in body.chars() {
        if out.len() + ch.len_utf8() > MAX_BODY_BYTES {
            break;
        }
        out.push(ch);
    }
    Some(out)
}

/// Build the ordered list of segments. Missing files are skipped silently —
/// AGENTS.md is opt-in at every layer.
pub fn build_stack(project_root: &Path, cwd: Option<&Path>) -> Vec<AgentsDocSegment> {
    let mut out: Vec<AgentsDocSegment> = Vec::with_capacity(5);

    if let Some(home) = dirs::home_dir() {
        let cortex_global = home.join(".cortex").join("AGENTS.md");
        if let Some(body) = read_capped(&cortex_global) {
            out.push(AgentsDocSegment { path: cortex_global, body, scope: "global" });
        }
        let codex_global = home.join(".codex").join("AGENTS.md");
        if let Some(body) = read_capped(&codex_global) {
            out.push(AgentsDocSegment { path: codex_global, body, scope: "codex" });
        }
    }

    let project_agents = project_root.join("AGENTS.md");
    if let Some(body) = read_capped(&project_agents) {
        out.push(AgentsDocSegment { path: project_agents, body, scope: "project" });
    }

    let cortex_local = project_root.join(".cortex").join("AGENTS.md");
    if let Some(body) = read_capped(&cortex_local) {
        out.push(AgentsDocSegment { path: cortex_local, body, scope: "cortex" });
    }

    // Sub-directory scope: only fires when `cwd` is *inside* the project root
    // and not the root itself. Codex's spec also recommends `cwd` AGENTS.md
    // entries shadow ancestors — we represent that by listing it last so the
    // merged view (and any naive concat reader) sees it as the final word.
    if let Some(c) = cwd {
        let canon_cwd = std::fs::canonicalize(c).unwrap_or_else(|_| c.to_path_buf());
        let canon_root = std::fs::canonicalize(project_root)
            .unwrap_or_else(|_| project_root.to_path_buf());
        let is_inside = canon_cwd.starts_with(&canon_root) && canon_cwd != canon_root;
        if is_inside {
            let cwd_agents = canon_cwd.join("AGENTS.md");
            if let Some(body) = read_capped(&cwd_agents) {
                out.push(AgentsDocSegment { path: cwd_agents, body, scope: "cwd" });
            }
        }
    }

    out
}

/// Concatenate segments with a scope header so models can tell layers apart.
/// Returns an empty string when no AGENTS.md files exist anywhere — callers
/// should check `is_empty()` rather than always prepending.
pub fn merged_text(segments: &[AgentsDocSegment]) -> String {
    if segments.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::with_capacity(segments.len());
    for seg in segments {
        parts.push(format!(
            "# [{}] {}\n\n{}",
            seg.scope,
            seg.path.display(),
            seg.body.trim_end(),
        ));
    }
    parts.join("\n\n---\n\n")
}

// ── Tauri commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn agents_md_stack(
    project_root: String,
    cwd: Option<String>,
) -> Result<Vec<AgentsDocSegment>, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = PathBuf::from(&project_root);
    let cwd_pb = cwd.map(PathBuf::from);
    Ok(build_stack(&root, cwd_pb.as_deref()))
}

#[tauri::command]
pub async fn agents_md_merged(
    project_root: String,
    cwd: Option<String>,
) -> Result<String, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = PathBuf::from(&project_root);
    let cwd_pb = cwd.map(PathBuf::from);
    Ok(merged_text(&build_stack(&root, cwd_pb.as_deref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_files_yield_empty_stack() {
        let tmp = tempfile::tempdir().unwrap();
        let stack = build_stack(tmp.path(), None);
        // The home-dir entries may exist for the test runner — but project /
        // cwd ones definitely don't, so the stack length is bounded above.
        for seg in &stack {
            assert_ne!(seg.scope, "project");
            assert_ne!(seg.scope, "cortex");
            assert_ne!(seg.scope, "cwd");
        }
    }

    #[test]
    fn project_and_cortex_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "repo-wide").unwrap();
        fs::create_dir_all(tmp.path().join(".cortex")).unwrap();
        fs::write(tmp.path().join(".cortex").join("AGENTS.md"), "cortex-only").unwrap();
        let stack = build_stack(tmp.path(), None);
        let scopes: Vec<&str> = stack.iter().map(|s| s.scope).collect();
        // .cortex/AGENTS.md is meant to override the repo AGENTS.md — it
        // therefore comes after `project` in the ordered stack.
        let pi = scopes.iter().position(|s| *s == "project").unwrap();
        let ci = scopes.iter().position(|s| *s == "cortex").unwrap();
        assert!(ci > pi, "cortex override must follow project");
    }

    #[test]
    fn cwd_only_when_inside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("packages").join("ui");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "ui-scoped").unwrap();

        let stack = build_stack(tmp.path(), Some(&sub));
        assert!(stack.iter().any(|s| s.scope == "cwd"));

        // Same cwd == project root: must NOT add a cwd segment.
        let stack_root = build_stack(tmp.path(), Some(tmp.path()));
        assert!(!stack_root.iter().any(|s| s.scope == "cwd"));

        // cwd outside the project: must NOT add either.
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("AGENTS.md"), "elsewhere").unwrap();
        let stack_out = build_stack(tmp.path(), Some(other.path()));
        assert!(!stack_out.iter().any(|s| s.scope == "cwd"));
    }

    #[test]
    fn body_capped_at_16_kib() {
        let tmp = tempfile::tempdir().unwrap();
        let huge = "a".repeat(MAX_BODY_BYTES + 1024);
        fs::write(tmp.path().join("AGENTS.md"), &huge).unwrap();
        let stack = build_stack(tmp.path(), None);
        let proj = stack.iter().find(|s| s.scope == "project").unwrap();
        assert!(proj.body.len() <= MAX_BODY_BYTES);
    }

    #[test]
    fn merged_text_emits_scope_headers() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "hello").unwrap();
        let stack = build_stack(tmp.path(), None);
        let merged = merged_text(&stack);
        assert!(merged.contains("[project]"));
        assert!(merged.contains("hello"));
    }

    #[test]
    fn merged_text_empty_when_no_segments() {
        assert_eq!(merged_text(&[]), "");
    }
}
