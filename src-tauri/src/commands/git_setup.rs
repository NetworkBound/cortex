//! Self-setup wizard backend: point Cortex at an Obsidian vault or a git
//! server, configured from inside the app.
//!
//! Three concerns live here:
//!   1. Validation of user-supplied git URLs and local paths (pure logic,
//!      unit-tested) so the UI can show inline ✓/✗ before any action.
//!   2. Cloning a remote repo via a thin `git clone` shell-out that mirrors
//!      [`crate::commands::git_pull`] (spawned through [`crate::sys::no_window`],
//!      stdout/stderr tail-trimmed to keep the IPC payload small).
//!   3. Persisting the chosen git-server URL + cloned path to
//!      `~/.cortex/git-config.json` (mirrors the `last-project.json` pattern in
//!      [`crate::app_state`]) and updating the live [`Config`].
//!
//! Every handler returns `Result<_, String>` with user-facing messages to match
//! the rest of the command bus. URLs/paths are never spawned as flags, and
//! credentials embedded in URLs are never logged.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::app_state::AppState;

/// Event emitted whenever the set of known projects changes (a repo was
/// cloned/connected and registered). The Projects sidebar listens and
/// re-fetches `list_projects`.
const PROJECTS_CHANGED_EVENT: &str = "projects:changed";

/// Tail length for stdout / stderr blobs returned to the frontend. Clone output
/// (progress + remote messages) can be wordy; mirror git_pull's budget.
const TAIL_BYTES: usize = 4 * 1024;

/// Result of validating a git remote URL. `normalized_url` strips a trailing
/// slash; `hostname` is best-effort extracted for display.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GitUrlInfo {
    pub is_valid: bool,
    pub normalized_url: String,
    pub hostname: String,
}

/// Result of a `git clone`. `ok` is `true` iff git exited 0; stdout / stderr are
/// tail-trimmed. On success `project_root` is the canonical path the repo was
/// registered under — the frontend's "Open project" hand-off keys off it.
#[derive(Debug, Clone, Serialize)]
pub struct CloneResult {
    pub ok: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_code: i32,
    pub project_root: Option<String>,
}

/// Result of inspecting a candidate Obsidian vault directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VaultInfo {
    pub path: String,
    pub is_valid: bool,
    pub is_obsidian_vault: bool,
}

/// Pure URL validation, factored out so it's unit-testable without IPC.
///
/// Accepts the common git transports: `https://`, `http://`, `ssh://`,
/// `git://`, `file://`, and scp-style `git@host:org/repo.git`. The host portion
/// is extracted best-effort for the UI to display.
fn validate_url(url: &str) -> GitUrlInfo {
    let trimmed = url.trim();
    let normalized = trimmed.trim_end_matches('/').to_string();

    if trimmed.is_empty() {
        return GitUrlInfo {
            is_valid: false,
            normalized_url: normalized,
            hostname: String::new(),
        };
    }

    // scp-style: git@host:path or user@host:path (no scheme, single colon).
    if let Some((before, after)) = trimmed.split_once(':') {
        if !before.contains("//") && before.contains('@') && !after.is_empty() {
            let host = before.rsplit('@').next().unwrap_or("").to_string();
            let valid = !host.is_empty();
            return GitUrlInfo {
                is_valid: valid,
                normalized_url: normalized,
                hostname: host,
            };
        }
    }

    const SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "file://"];
    for scheme in SCHEMES {
        if let Some(rest) = trimmed.strip_prefix(scheme) {
            if scheme == "file://" {
                // file:// has no hostname; a non-empty path is enough.
                return GitUrlInfo {
                    is_valid: !rest.is_empty(),
                    normalized_url: normalized,
                    hostname: String::new(),
                };
            }
            // Host is everything up to the first '/', stripping any userinfo@.
            let authority = rest.split('/').next().unwrap_or("");
            let host = authority.rsplit('@').next().unwrap_or("");
            // Drop a :port suffix for the displayed hostname.
            let host = host.split(':').next().unwrap_or("");
            return GitUrlInfo {
                is_valid: !host.is_empty(),
                normalized_url: normalized,
                hostname: host.to_string(),
            };
        }
    }

    GitUrlInfo {
        is_valid: false,
        normalized_url: normalized,
        hostname: String::new(),
    }
}

/// Validate a git remote URL for the setup UI. Never spawns anything.
#[tauri::command]
pub async fn validate_git_url(url: String) -> Result<GitUrlInfo, String> {
    Ok(validate_url(&url))
}

/// `git clone <url> <target_dir>`. The parent of `target_dir` must exist and be
/// a directory; `target_dir` itself must not already exist (git refuses to clone
/// into a non-empty dir, but we pre-check for a clearer error). On success the
/// URL + resulting path are persisted via [`AppState`].
#[tauri::command]
pub async fn clone_git_repo(
    url: String,
    target_dir: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CloneResult, String> {
    let info = validate_url(&url);
    if !info.is_valid {
        return Err("Invalid git URL format".into());
    }

    let target = PathBuf::from(&target_dir);
    let parent = target
        .parent()
        .ok_or_else(|| "Target directory has no parent".to_string())?;
    if !parent.is_dir() {
        return Err(format!(
            "Parent directory does not exist: {}",
            parent.display()
        ));
    }
    if target.exists() {
        // Allow an empty existing dir; git itself rejects non-empty ones.
        let empty = std::fs::read_dir(&target)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(format!(
                "Target directory already exists and is not empty: {target_dir}"
            ));
        }
    }

    let output = crate::sys::no_window("git")
        .arg("clone")
        .arg(&info.normalized_url)
        .arg(&target_dir)
        .output()
        .map_err(|e| format!("git clone: spawn failed (is git installed?): {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let ok = output.status.success();

    let mut project_root = None;
    if ok {
        // Persist URL + path, register the repo as a project, and tell the
        // Projects sidebar to refresh. All best-effort: a persistence failure
        // shouldn't discard a successful clone, so failures are surfaced in
        // stderr_tail instead of failing the whole command.
        let canonical = target.canonicalize().unwrap_or(target.clone());
        if let Err(e) = AppState::save_git_server(Some(&info.normalized_url), Some(&canonical)) {
            stderr.push_str(&format!(
                "\n[warn] clone succeeded but saving config failed: {e}"
            ));
        }
        if let Err(e) = crate::projects::register_project_path(&canonical) {
            stderr.push_str(&format!(
                "\n[warn] clone succeeded but registering the project failed: {e}"
            ));
        }
        {
            let mut cfg = state.config.write();
            cfg.git_server_url = Some(info.normalized_url.clone());
            cfg.git_server_cloned_path = Some(canonical.clone());
        }
        let _ = app.emit(PROJECTS_CHANGED_EVENT, ());
        project_root = Some(canonical.to_string_lossy().into_owned());
    }

    Ok(CloneResult {
        ok,
        stdout_tail: tail(stdout, TAIL_BYTES),
        stderr_tail: tail(stderr, TAIL_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
        project_root,
    })
}

/// Inspect a candidate Obsidian vault directory: must exist and be a directory;
/// a `.obsidian` subdir confirms it's a real vault (`is_obsidian_vault`).
#[tauri::command]
pub async fn validate_obsidian_vault(path: String) -> Result<VaultInfo, String> {
    let p = PathBuf::from(&path);
    let is_dir = p.is_dir();
    let is_vault = is_dir && p.join(".obsidian").is_dir();
    Ok(VaultInfo {
        path,
        is_valid: is_dir,
        is_obsidian_vault: is_vault,
    })
}

/// Validate + persist the git-server URL without cloning (e.g. user just wants
/// to record where their Gitea/GitHub lives).
#[tauri::command]
pub async fn set_git_server_url(url: String, state: State<'_, AppState>) -> Result<(), String> {
    let info = validate_url(&url);
    if !info.is_valid {
        return Err("Invalid git URL format".into());
    }
    AppState::save_git_server(Some(&info.normalized_url), None).map_err(|e| e.to_string())?;
    state.config.write().git_server_url = Some(info.normalized_url);
    Ok(())
}

/// Connect an already-cloned local repo: the path must exist, be a directory and
/// contain a `.git` entry. Persists the path, registers it as a project, and
/// returns the canonical path (the frontend's "Open project" hand-off keys
/// off it).
#[tauri::command]
pub async fn set_git_server_cloned_path(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(format!("Directory does not exist: {path}"));
    }
    // `.git` may be a dir (normal repo) or a file (worktree/submodule link).
    if !p.join(".git").exists() {
        return Err(format!("Not a git repository (no .git found): {path}"));
    }
    let canonical = p.canonicalize().unwrap_or(p);
    AppState::save_git_server(None, Some(&canonical)).map_err(|e| e.to_string())?;
    // Registration is what makes the repo show up in the Projects sidebar —
    // a failure here is worth surfacing, unlike the best-effort config write.
    crate::projects::register_project_path(&canonical)
        .map_err(|e| format!("connected, but registering the project failed: {e}"))?;
    state.config.write().git_server_cloned_path = Some(canonical.clone());
    let _ = app.emit(PROJECTS_CHANGED_EVENT, ());
    Ok(canonical.to_string_lossy().into_owned())
}

/// Truncate from the front, keeping the last `limit` bytes. Mirrors git_pull's
/// `tail` — clone output is most interesting at the bottom. Respects UTF-8
/// boundaries.
fn tail(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = s.len() - limit;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.replace_range(..cut, "[…truncated…]\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_https_url() {
        let info = validate_url("https://github.com/owner/repo.git");
        assert!(info.is_valid);
        assert_eq!(info.hostname, "github.com");
        assert_eq!(info.normalized_url, "https://github.com/owner/repo.git");
    }

    #[test]
    fn strips_trailing_slash_and_userinfo_for_host() {
        let info = validate_url("https://token@gitea.example.com:3000/o/r.git/");
        assert!(info.is_valid);
        assert_eq!(info.hostname, "gitea.example.com");
        assert_eq!(
            info.normalized_url,
            "https://token@gitea.example.com:3000/o/r.git"
        );
    }

    #[test]
    fn validates_scp_style_url() {
        let info = validate_url("git@github.com:owner/repo.git");
        assert!(info.is_valid);
        assert_eq!(info.hostname, "github.com");
    }

    #[test]
    fn validates_ssh_scheme() {
        let info = validate_url("ssh://git@host.tld/path/repo.git");
        assert!(info.is_valid);
        assert_eq!(info.hostname, "host.tld");
    }

    #[test]
    fn validates_file_scheme() {
        let info = validate_url("file:///srv/git/repo.git");
        assert!(info.is_valid);
        assert_eq!(info.hostname, "");
    }

    #[test]
    fn rejects_empty_and_garbage() {
        assert!(!validate_url("").is_valid);
        assert!(!validate_url("   ").is_valid);
        assert!(!validate_url("not a url").is_valid);
        assert!(!validate_url("https://").is_valid);
    }

    #[test]
    fn tail_returns_short_string_intact() {
        let s = "abc".to_string();
        assert_eq!(tail(s.clone(), 100), s);
    }

    #[test]
    fn tail_keeps_last_chunk_with_marker() {
        let s = "x".repeat(TAIL_BYTES + 200);
        let out = tail(s, TAIL_BYTES);
        assert!(out.starts_with("[…truncated…]"));
        assert!(out.ends_with('x'));
    }
}
