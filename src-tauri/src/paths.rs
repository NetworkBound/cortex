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
