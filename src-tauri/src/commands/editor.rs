//! Editor file-save backend.
//!
//! The inline CodeMirror pane (`components/EditorPane.tsx`) opens files the
//! user has explicitly clicked on — the file explorer, slash-command picker,
//! or `cortex:editor-open` event. Because the pane never auto-opens anything
//! the user didn't authorize, the save path here intentionally accepts any
//! absolute path the user could open: scoping happens at *open* time, not at
//! save time.
//!
//! Guards we still apply:
//!   - reject empty paths,
//!   - reject relative paths (must be absolute),
//!   - cap body size at 10 MiB so a runaway buffer can't fill the disk,
//!   - reject paths containing NUL bytes.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Maximum text-buffer size we will persist. Editor files are source code, not
/// blobs — 10 MiB is already orders of magnitude beyond anything reasonable.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Resolve the user's home directory. Uses `dirs::home_dir()` so this works on
/// Windows (`%USERPROFILE%`) as well as POSIX (`$HOME`) — a bare `$HOME` lookup
/// is unset on most Windows hosts, which would make `confine_to_home` fail
/// closed and disable file saving entirely.
fn home_dir() -> Option<PathBuf> {
    dirs::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// Lexically normalize an absolute path, collapsing `.`/`..` without touching
/// the filesystem. The caller guarantees the path is absolute.
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

/// Confine writes to the user's home directory so a compromised renderer can't
/// turn `save_file_text` into an arbitrary-file-write primitive (overwriting
/// shell rc files, SSH config, the app binary, etc.). We normalize away
/// `..`/`.` traversal first, then require the result to live under `$HOME`.
/// If `$HOME` is unknown we fail closed.
fn confine_to_home(pb: &Path) -> Result<PathBuf, String> {
    let normalized = lexical_normalize(pb);
    let home = home_dir().ok_or("cannot determine home directory; refusing write")?;
    let home = lexical_normalize(&home);
    if !normalized.starts_with(&home) {
        return Err(format!(
            "refusing to write outside home directory: {}",
            normalized.display()
        ));
    }
    Ok(normalized)
}

/// Read a text file's contents. Counterpart to `save_file_text`.
/// Frontend (`lib/multibuffer.ts`, `EditorPane`) calls this first and falls
/// back to `@tauri-apps/plugin-fs::readTextFile` on error — keeping that
/// graceful-degradation path. Same guards as `save_file_text`: absolute
/// path required, NUL rejected, body capped at 10 MiB.
#[tauri::command]
pub async fn read_file_text(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        if path.is_empty() {
            return Err("path is empty".into());
        }
        if path.contains('\0') {
            return Err("path contains NUL".into());
        }
        let pb = PathBuf::from(&path);
        if !pb.is_absolute() {
            return Err(format!("path must be absolute: {path}"));
        }
        let meta = fs::metadata(&pb).map_err(|e| format!("stat {}: {e}", pb.display()))?;
        if meta.len() as usize > MAX_BODY_BYTES {
            return Err(format!(
                "file exceeds {} MiB limit",
                MAX_BODY_BYTES / (1024 * 1024)
            ));
        }
        fs::read_to_string(&pb).map_err(|e| format!("read {}: {e}", pb.display()))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn save_file_text(path: String, body: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        if path.is_empty() {
            return Err("path is empty".into());
        }
        if path.contains('\0') {
            return Err("path contains NUL".into());
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(format!(
                "body exceeds {} MiB limit",
                MAX_BODY_BYTES / (1024 * 1024)
            ));
        }
        let pb = PathBuf::from(&path);
        if !pb.is_absolute() {
            return Err(format!("path must be absolute: {path}"));
        }
        let pb = confine_to_home(&pb)?;
        fs::write(&pb, body).map_err(|e| format!("write {}: {e}", pb.display()))?;
        Ok(pb.display().to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rejects_empty_path() {
        let err =
            tauri::async_runtime::block_on(save_file_text(String::new(), "hi".into())).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn rejects_relative_path() {
        let err = tauri::async_runtime::block_on(save_file_text("foo.txt".into(), "hi".into()))
            .unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn rejects_oversized_body() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("big.txt").display().to_string();
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        // Oversized check happens before the home-confinement guard, so this
        // fails regardless of HOME.
        let err = tauri::async_runtime::block_on(save_file_text(path, huge)).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn writes_absolute_path() {
        let td = TempDir::new().unwrap();
        // Point HOME at the temp dir so the write lands inside the confined root.
        std::env::set_var("HOME", td.path());
        let path = td.path().join("ok.txt");
        let body = "hello world\n".to_string();
        let out = tauri::async_runtime::block_on(save_file_text(
            path.display().to_string(),
            body.clone(),
        ))
        .unwrap();
        assert_eq!(out, path.display().to_string());
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn rejects_write_outside_home() {
        let home = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        let path = outside.path().join("evil.txt").display().to_string();
        let err =
            tauri::async_runtime::block_on(save_file_text(path, "pwned".into())).unwrap_err();
        assert!(err.contains("outside home"));
    }

    #[test]
    fn rejects_traversal_escape_from_home() {
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        // Absolute path that uses `..` to climb back out of home.
        let escape = home.path().join("../../../../etc/passwd");
        let err = tauri::async_runtime::block_on(save_file_text(
            escape.display().to_string(),
            "x".into(),
        ))
        .unwrap_err();
        assert!(err.contains("outside home"));
    }
}
