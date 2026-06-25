//! Project trust list — Codex-style explicit opt-in.
//!
//! Cortex defaults to **untrusted** for every project. While untrusted:
//!
//!   * The sandbox tier is force-pinned to `ReadOnly` regardless of
//!     `<project_root>/.cortex/sandbox.toml`.
//!   * Per-project rules (`.cortex/rules/*.md`, `.cortex/danger.toml`,
//!     `.cortex/approvals.toml`) are **not** loaded into the session
//!     context. Root-level `CLAUDE.md` etc. still load.
//!
//! Trust is **global**, stored at `~/.cortex/trusted-paths.json` as a JSON
//! array of absolute path strings:
//!
//! ```json
//! ["/home/user/projects/cortex", "/home/user/projects/other-repo"]
//! ```
//!
//! The file is read on every check (it's tiny and rarely contended). Writes
//! create `~/.cortex/` if missing and rewrite the file atomically via a
//! temp-file + rename so a crashed write can't corrupt the list.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Returns `~/.cortex/trusted-paths.json`, or `None` if the home directory
/// can't be resolved (rare, but we degrade to "nothing is trusted").
fn trust_file() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".cortex").join("trusted-paths.json"))
}

/// Normalize a project root for comparison/storage. We lexically canonicalize
/// (strip trailing slash, normalize separators) but do **not** touch the fs —
/// the path may not exist at check time and we don't want to follow symlinks
/// for a security gate.
fn normalize(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.trim_end_matches('/').to_string()
}

/// Read the on-disk trust list. Missing file / parse errors yield an empty
/// list (deny-bias: untrusted-by-default).
fn load_list() -> Vec<String> {
    let Some(path) = trust_file() else { return Vec::new() };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("trust: no list ({}): {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<String>>(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("trust: bad json at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Atomically persist the trust list. Creates `~/.cortex/` if missing.
fn save_list(list: &[String]) -> anyhow::Result<()> {
    let path = trust_file().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(list)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// True iff `project_root` (normalized) is in `~/.cortex/trusted-paths.json`.
/// Default state — file missing, project absent — is **false**.
pub fn is_trusted(project_root: &Path) -> bool {
    let target = normalize(project_root);
    if target.is_empty() {
        return false;
    }
    load_list().iter().any(|p| normalize(Path::new(p)) == target)
}

/// Add `project_root` to the trust list. Idempotent — re-trusting an
/// already-trusted path is a no-op (no error, no duplicate entry).
pub fn trust_path(project_root: &Path) -> anyhow::Result<()> {
    let target = normalize(project_root);
    if target.is_empty() {
        anyhow::bail!("trust: empty project root");
    }
    let mut list = load_list();
    if list.iter().any(|p| normalize(Path::new(p)) == target) {
        return Ok(());
    }
    list.push(target);
    save_list(&list)
}

/// Remove `project_root` from the trust list. Idempotent — untrusting an
/// already-untrusted path succeeds silently.
pub fn untrust_path(project_root: &Path) -> anyhow::Result<()> {
    let target = normalize(project_root);
    if target.is_empty() {
        return Ok(());
    }
    let mut list = load_list();
    let before = list.len();
    list.retain(|p| normalize(Path::new(p)) != target);
    if list.len() == before {
        return Ok(());
    }
    save_list(&list)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The trust list is global (`~/.cortex/...`), so we can't isolate it
    // cleanly inside `#[test]` without an env override. These tests cover the
    // pure helpers; integration tests live in commands/trust.rs.

    #[test]
    fn normalize_strips_trailing_slash_and_normalizes_separators() {
        assert_eq!(normalize(Path::new("/home/foo/")), "/home/foo");
        assert_eq!(normalize(Path::new("/home/foo")), "/home/foo");
        assert_eq!(normalize(Path::new("/home/foo//")), "/home/foo");
    }

    #[test]
    fn empty_root_is_never_trusted() {
        assert!(!is_trusted(Path::new("")));
    }
}
