//! Shared path helpers.
//!
//! `home_dir` exists because unit tests cannot redirect `dirs::home_dir()`
//! on Windows: dirs resolves the profile through the known-folder API, so
//! setting `$HOME`/`%USERPROFILE%` has no effect there. Tests instead set
//! `CORTEX_TEST_HOME` (via [`test_home::with_temp_home`]), which this
//! resolver honors only in `cfg(test)` builds.

use std::path::PathBuf;

/// Home directory used for Cortex state (`~/.cortex/...`).
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(h) = std::env::var_os("CORTEX_TEST_HOME") {
        return Some(PathBuf::from(h));
    }
    dirs::home_dir()
}

/// Strip the Windows "verbatim" prefix `std::fs::canonicalize` produces
/// (`\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` → `\\srv\share`). Everything
/// else — including every non-Windows path — is returned unchanged.
///
/// Verbatim paths are correct for the Win32 API but wrong almost everywhere a
/// path is persisted or shown: they don't compare equal to the plain spelling
/// the user (or `dirs`) hands us, and several tools (older git, node scripts,
/// `cmd.exe`) choke on them. Use this on anything that came out of
/// `canonicalize()` before storing or displaying it.
pub fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None => return p,
    };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // Only drive-letter forms are safe to unwrap; other `\\?\` shapes
        // (device paths, `\\?\Volume{…}`) stay as-is.
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    p
}

/// `canonicalize()` that never returns a verbatim (`\\?\`) path and degrades
/// to the input when the path can't be canonicalized (missing, permission).
pub fn canonicalize_lossy(p: &std::path::Path) -> PathBuf {
    match p.canonicalize() {
        Ok(c) => strip_verbatim_prefix(c),
        Err(_) => p.to_path_buf(),
    }
}

#[cfg(test)]
pub mod test_home {
    use std::path::Path;
    use std::sync::Mutex;

    /// One process-global lock: per-module locks can't stop two modules'
    /// tests from racing on the shared `CORTEX_TEST_HOME`/`HOME` env vars.
    static LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with the home dir redirected to a fresh temp dir. The temp
    /// path is passed to `f` for tests that need to seed files under it.
    /// Env vars are restored afterwards even if `f` panics elsewhere first
    /// poisoned the lock (poison is ignored — the env is re-set each call).
    pub fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("CORTEX_TEST_HOME", tmp.path());
        // Keep $HOME in sync for code paths that read it directly (unix).
        std::env::set_var("HOME", tmp.path());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(tmp.path())));
        std::env::remove_var("CORTEX_TEST_HOME");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        if let Err(p) = result {
            std::panic::resume_unwind(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_drive_letter_verbatim_prefix() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\me\proj")),
            PathBuf::from(r"C:\Users\me\proj")
        );
    }

    #[test]
    fn rewrites_verbatim_unc_to_plain_unc() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
    }

    #[test]
    fn leaves_other_paths_alone() {
        for p in [
            r"C:\plain\path",
            "/home/user/proj",
            r"\\server\share",
            r"\\?\Volume{abc}\x",
            "relative/dir",
        ] {
            assert_eq!(
                strip_verbatim_prefix(PathBuf::from(p)),
                PathBuf::from(p),
                "{p}"
            );
        }
    }

    #[test]
    fn canonicalize_lossy_falls_back_to_input_for_missing_paths() {
        let missing = std::path::Path::new("definitely/not/a/real/dir/xyzzy-42");
        assert_eq!(canonicalize_lossy(missing), missing.to_path_buf());
        // A real dir canonicalizes to an absolute, non-verbatim path.
        let tmp = tempfile::tempdir().unwrap();
        let c = canonicalize_lossy(tmp.path());
        assert!(c.is_absolute());
        assert!(!c.to_string_lossy().starts_with(r"\\?\"));
    }
}
