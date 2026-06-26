//! Project discovery + file tree. Phase 5 walks `~/projects/*` looking for
//! git repos and `CLAUDE.md` markers, and surfaces a lazy file tree the
//! frontend can request on demand.

pub mod diagnostics;
pub mod ignore;
pub mod rules;

use crate::projects::ignore::CortexIgnore;
use serde::Serialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMeta {
    pub root: PathBuf,
    pub name: String,
    pub has_claude_md: bool,
    pub has_git: bool,
    pub has_runbooks: bool,
    pub last_modified_ms: i64,
    /// Section header the frontend groups rows under (e.g. "Code",
    /// "Vault Projects", or an intermediate folder name).
    pub group: String,
    /// "code" for filesystem repos, "vault" for Obsidian project notes.
    pub kind: String,
    /// For vault notes: the backing `.md` file to load as context. `None`
    /// for plain code dirs and vault subdirs without an index note.
    pub note_path: Option<PathBuf>,
    /// Optional short hint shown under the name (e.g. relative path).
    pub subtitle: Option<String>,
}

/// File holding explicitly-registered project roots (repos cloned/connected
/// via the Setup wizard, or any future "add folder as project" surface).
/// Shape: `{ "projects": ["/abs/path", ...] }`. Registered roots surface in
/// discovery regardless of where they live on disk — the fix for Setup's
/// "Clone & connect" dead end, where a repo cloned outside `~/projects`
/// never appeared anywhere in the app.
pub fn registered_projects_file() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("registered-projects.json"))
}

fn load_registered_from(file: &Path) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    v.get("projects")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn save_registered_to(file: &Path, paths: &[PathBuf]) -> anyhow::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::json!({
        "projects": paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    });
    std::fs::write(file, serde_json::to_vec_pretty(&json)?)?;
    Ok(())
}

/// Register `dir` (must exist) in the project registry. Idempotent; prunes
/// entries whose directories no longer exist while it's there. Returns `true`
/// when the path was newly added.
pub fn register_project_path(dir: &Path) -> anyhow::Result<bool> {
    let file =
        registered_projects_file().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    register_project_path_in(&file, dir)
}

fn register_project_path_in(file: &Path, dir: &Path) -> anyhow::Result<bool> {
    let canonical = dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot resolve {}: {e}", dir.display()))?;
    let mut list = load_registered_from(file);
    list.retain(|p| p.is_dir());
    if list.iter().any(|p| p == &canonical) {
        save_registered_to(file, &list)?;
        return Ok(false);
    }
    list.push(canonical);
    save_registered_to(file, &list)?;
    Ok(true)
}

/// Remove `dir` from the registry (matched by canonical path when resolvable,
/// else by literal equality — the dir may already be deleted). Returns `true`
/// when an entry was removed.
pub fn unregister_project_path(dir: &Path) -> anyhow::Result<bool> {
    let file =
        registered_projects_file().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    unregister_project_path_in(&file, dir)
}

fn unregister_project_path_in(file: &Path, dir: &Path) -> anyhow::Result<bool> {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut list = load_registered_from(file);
    let before = list.len();
    list.retain(|p| p != dir && p != &canonical);
    if list.len() == before {
        return Ok(false);
    }
    save_registered_to(file, &list)?;
    Ok(true)
}

/// Resolve the Obsidian vault root: honor `OBSIDIAN_VAULT` if set, else
/// `~/vault`.
pub fn vault_root() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("OBSIDIAN_VAULT") {
        let p = PathBuf::from(v.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join("vault"))
}

fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Discover all projects: filesystem code repos plus Obsidian vault project
/// notes. `vault_root` is the resolved Obsidian vault path (normally
/// `AppState.config.obsidian_vault`); pass `None` to fall back to the
/// env-var / `~/vault` heuristic via [`vault_root()`].
pub fn discover_projects(vault_root: Option<PathBuf>) -> Vec<ProjectMeta> {
    let mut out = discover_code_projects();
    let vault = vault_root.or_else(self::vault_root);
    out.extend(discover_vault_projects(vault));
    out.sort_by(|a, b| b.last_modified_ms.cmp(&a.last_modified_ms));
    out
}

/// Walk each configured root (plus `~/projects`) up to depth 3, treating a
/// directory as a project when it has `.git`, `CLAUDE.md`, or `runbooks`.
/// Matched directories are NOT descended into so nested repos/submodules
/// don't each surface as their own row.
fn discover_code_projects() -> Vec<ProjectMeta> {
    // Scan EVERY reachable home so cortex.exe on Windows can see WSL-side
    // projects in addition to its native `C:\Users\<user>\projects`. Without
    // this, user's sidebar shows empty on Windows because all his repos
    // live at `/home/user/projects` on WSL.
    let mut roots: Vec<PathBuf> = Vec::new();
    // Explicit override wins and is added first so its entries take precedence
    // on any path-level de-dup below. Supports the documented
    // CORTEX_PROJECTS_ROOT escape hatch for users whose repos don't live in
    // ~/projects. The value is a list split on ':' and ',' so multiple roots
    // can be configured at once.
    if let Ok(custom) = std::env::var("CORTEX_PROJECTS_ROOT") {
        for part in custom.split([':', ',']) {
            let p = PathBuf::from(part.trim());
            if !p.as_os_str().is_empty() && !roots.iter().any(|r| r == &p) {
                roots.push(p);
            }
        }
    }
    if let Some(h) = dirs::home_dir() {
        let dflt = h.join("projects");
        if !roots.iter().any(|r| r == &dflt) {
            roots.push(dflt);
        }
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME")
            .ok()
            .map(|u| u.to_lowercase())
            .unwrap_or_else(|| "user".to_string());
        for distro in ["Ubuntu", "Ubuntu-24.04", "Ubuntu-22.04", "Debian"] {
            let p = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\home\\{user}\\projects"));
            if p.exists() && !roots.iter().any(|r| r == &p) {
                roots.push(p);
            }
        }
    }

    let is_project = |p: &Path| -> bool {
        p.join(".git").exists() || p.join("CLAUDE.md").exists() || p.join("runbooks").exists()
    };

    let mut out: Vec<ProjectMeta> = Vec::new();
    // Canonical paths of dirs already accepted as projects — used both for
    // cross-root de-dup AND to skip descending into a matched subtree.
    let mut matched: Vec<PathBuf> = Vec::new();

    // Explicitly-registered projects (Setup wizard clone/connect) come first,
    // wherever they live; the scan below de-dups against them via `matched`.
    // Dead entries are skipped here and pruned on the next registration.
    if let Some(file) = registered_projects_file() {
        for path in load_registered_from(&file) {
            if !path.is_dir() {
                continue;
            }
            let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if matched.iter().any(|m| &canon == m || canon.starts_with(m)) {
                continue;
            }
            out.push(ProjectMeta {
                name: path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                has_claude_md: path.join("CLAUDE.md").exists(),
                has_git: path.join(".git").exists(),
                has_runbooks: path.join("runbooks").exists(),
                last_modified_ms: modified_ms(&path),
                group: "Code".to_string(),
                kind: "code".to_string(),
                note_path: None,
                subtitle: None,
                root: path,
            });
            matched.push(canon);
        }
    }
    for projects_dir in &roots {
        if !projects_dir.exists() { continue; }
        let root_canon = std::fs::canonicalize(projects_dir).unwrap_or_else(|_| projects_dir.clone());
        // Collect candidate dirs shallowest-first so a parent project is
        // recorded before any nested repo/submodule under it. We then prune
        // descendants of already-matched dirs as we go (the borrow checker
        // forbids mutating `matched` from inside WalkDir's `filter_entry`).
        let mut candidates: Vec<PathBuf> = WalkDir::new(projects_dir)
            .max_depth(3)
            .sort_by(|a, b| a.depth().cmp(&b.depth()))
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.depth() > 0 && e.file_type().is_dir())
            .map(|e| e.into_path())
            .collect();
        candidates.sort_by_key(|p| p.components().count());
        for path in &candidates {
            let path = path.as_path();
            if !is_project(path) { continue; }
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            // Skip if this dir is the same as, or nested under, a dir we've
            // already recorded as a project (or reached via another root).
            if matched.iter().any(|m| &canon == m || canon.starts_with(m)) { continue; }
            // Group: repos directly under a root are "Code"; deeper ones use
            // their nearest intermediate folder name relative to the root.
            let rel = canon.strip_prefix(&root_canon).ok();
            let group = match rel {
                Some(r) => {
                    let comps: Vec<_> = r.components().collect();
                    if comps.len() <= 1 {
                        "Code".to_string()
                    } else {
                        comps[comps.len() - 2]
                            .as_os_str()
                            .to_string_lossy()
                            .to_string()
                    }
                }
                None => "Code".to_string(),
            };
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(ProjectMeta {
                name,
                has_claude_md: path.join("CLAUDE.md").exists(),
                has_git: path.join(".git").exists(),
                has_runbooks: path.join("runbooks").exists(),
                last_modified_ms: modified_ms(path),
                group,
                kind: "code".to_string(),
                note_path: None,
                subtitle: rel.map(|r| r.to_string_lossy().to_string()),
                root: path.to_path_buf(),
            });
            matched.push(canon);
        }
    }
    out
}

/// Surface the user's Obsidian "project notes": top-level `*.md` files and
/// subdirectories under `<vault>/30-Projects`. Each becomes a `kind="vault"`
/// project grouped under "Vault Projects". Missing dir → empty list (no error).
fn discover_vault_projects(vault_root: Option<PathBuf>) -> Vec<ProjectMeta> {
    let mut out = Vec::new();
    let Some(vault) = vault_root else { return out; };
    let dir = vault.join("30-Projects");
    if !dir.is_dir() {
        return out;
    }
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.is_file() {
            // Top-level note: must be a `.md`, excluding `index.md`.
            let is_md = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if !is_md {
                continue;
            }
            if file_name.eq_ignore_ascii_case("index.md") {
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let name = stem
                .strip_prefix("Project - ")
                .map(|s| s.to_string())
                .unwrap_or(stem);
            out.push(ProjectMeta {
                name,
                has_claude_md: false,
                has_git: false,
                has_runbooks: false,
                last_modified_ms: modified_ms(&path),
                group: "Vault Projects".to_string(),
                kind: "vault".to_string(),
                note_path: Some(path.clone()),
                subtitle: Some(file_name),
                root: path,
            });
        } else if path.is_dir() {
            // Subdir project: optional same-named or index note inside.
            let same_named = path.join(format!("{file_name}.md"));
            let index_note = path.join("index.md");
            let note_path = if same_named.is_file() {
                Some(same_named)
            } else if index_note.is_file() {
                Some(index_note)
            } else {
                None
            };
            out.push(ProjectMeta {
                name: file_name.clone(),
                has_claude_md: false,
                has_git: false,
                has_runbooks: false,
                last_modified_ms: modified_ms(&path),
                group: "Vault Projects".to_string(),
                kind: "vault".to_string(),
                note_path,
                subtitle: Some(format!("{file_name}/")),
                root: path,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct FileTreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: Option<u64>,
}

pub fn list_files(root: &Path, max_entries: usize) -> Vec<FileTreeEntry> {
    let ignore = CortexIgnore::load(root);
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(2)
        .into_iter()
        .filter_entry(|e| {
            // Always-on hidden/build-output denylist still wins so we don't
            // descend into `.git/` etc even if .cortexignore is missing.
            // Wave 173 — don't apply the dot-prefix rejection to the root
            // itself; tempdir names start with `.tmpXXX` and were filtering
            // their own descendants out, surfacing as a spaces test failure
            // (and probably more subtle runtime issues for users with
            // tempdir-like project locations).
            if e.depth() == 0 { return true; }
            let name = e.file_name().to_string_lossy();
            if name.starts_with('.') { return false; }
            !ignore.is_denied(e.path(), root)
        })
        .filter_map(|e| e.ok())
    {
        if entry.depth() == 0 { continue; }
        if out.len() >= max_entries { break; }
        let is_dir = entry.file_type().is_dir();
        let size = if is_dir { None } else { entry.metadata().ok().map(|m| m.len()) };
        out.push(FileTreeEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir,
            size_bytes: size,
            path: entry.path().to_path_buf(),
        });
    }
    out.sort_by(|a, b| (!a.is_dir).cmp(&!b.is_dir).then(a.name.cmp(&b.name)));
    out
}

/// Status of `.cortexignore` for a project — surfaced in the UI so users
/// know whether their deny-list is loaded.
#[derive(Debug, Clone, Serialize)]
pub struct CortexIgnoreStatus {
    pub project_root: PathBuf,
    pub has_user_patterns: bool,
    pub user_pattern_count: usize,
    pub global_path: Option<PathBuf>,
    pub project_path: PathBuf,
    pub project_exists: bool,
    pub global_exists: bool,
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn register_load_roundtrip_and_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("registered-projects.json");
        let proj = tmp.path().join("repo-a");
        std::fs::create_dir_all(&proj).unwrap();

        assert!(register_project_path_in(&file, &proj).unwrap());
        // Second registration of the same dir is a no-op.
        assert!(!register_project_path_in(&file, &proj).unwrap());

        let loaded = load_registered_from(&file);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], proj.canonicalize().unwrap());
    }

    #[test]
    fn register_rejects_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("registered-projects.json");
        assert!(register_project_path_in(&file, &tmp.path().join("nope")).is_err());
        assert!(load_registered_from(&file).is_empty());
    }

    #[test]
    fn register_prunes_dead_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("registered-projects.json");
        let a = tmp.path().join("repo-a");
        let b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        register_project_path_in(&file, &a).unwrap();
        register_project_path_in(&file, &b).unwrap();

        std::fs::remove_dir_all(&a).unwrap();
        // Registering anything prunes entries whose dirs no longer exist.
        let c = tmp.path().join("repo-c");
        std::fs::create_dir_all(&c).unwrap();
        register_project_path_in(&file, &c).unwrap();

        let loaded = load_registered_from(&file);
        assert_eq!(loaded.len(), 2);
        assert!(!loaded.iter().any(|p| p.ends_with("repo-a")));
    }

    #[test]
    fn unregister_removes_even_deleted_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("registered-projects.json");
        let a = tmp.path().join("repo-a");
        std::fs::create_dir_all(&a).unwrap();
        register_project_path_in(&file, &a).unwrap();
        let canonical = a.canonicalize().unwrap();
        std::fs::remove_dir_all(&a).unwrap();

        // The dir is gone, so canonicalize fails — must still match the
        // stored (canonical) entry.
        assert!(unregister_project_path_in(&file, &canonical).unwrap());
        assert!(load_registered_from(&file).is_empty());
        assert!(!unregister_project_path_in(&file, &canonical).unwrap());
    }

    #[test]
    fn load_tolerates_missing_and_malformed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("absent.json");
        assert!(load_registered_from(&missing).is_empty());
        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, b"not json").unwrap();
        assert!(load_registered_from(&bad).is_empty());
    }
}

pub fn ignore_status(project_root: &Path) -> CortexIgnoreStatus {
    let ignore = CortexIgnore::load(project_root);
    let project_path = project_root.join(".cortexignore");
    let project_exists = project_path.exists();
    let (global_path, global_exists) = match dirs::home_dir() {
        Some(h) => {
            let p = h.join(".cortex").join("cortexignore");
            let exists = p.exists();
            (Some(p), exists)
        }
        None => (None, false),
    };
    CortexIgnoreStatus {
        project_root: project_root.to_path_buf(),
        has_user_patterns: ignore.has_user_patterns(),
        user_pattern_count: ignore.user_pattern_count,
        global_path,
        project_path,
        project_exists,
        global_exists,
    }
}
