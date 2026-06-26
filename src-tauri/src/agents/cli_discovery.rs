//! Reusable local-CLI binary discovery.
//!
//! Generalizes the original `claude_cli::claude_bin()` logic so ANY headless AI
//! CLI (claude, codex, gemini, …) can be located the same way, with no homelab
//! dependency. The lookup order is, for each candidate file name:
//!
//!   1. The conventional `~/.local/bin/<name>` install location.
//!   2. Any caller-supplied extra directories (e.g. the Windows npm global
//!      prefix `%APPDATA%\npm`), tried in order.
//!   3. A manual `$PATH` scan (so Windows executable extensions resolve without
//!      going through a shell).
//!   4. A final `which::which` fallback per name.
//!
//! Candidate file names are per-OS and supplied by the caller: on Windows an npm
//! install lands as `<name>.cmd` / `<name>.exe` / `<name>.bat`, while POSIX uses
//! the bare name — a bare `claude` never matches a `.cmd` shim on Windows, so the
//! caller passes the full extension list.

use std::path::PathBuf;

/// A zero-arg directory provider. Returning `None` means "not applicable on this
/// platform / environment" and the directory is simply skipped. Using fns (not
/// precomputed `PathBuf`s) lets a spec be a `const`/`static` and defers any env
/// lookups (`%APPDATA%`, npm prefix) to call time.
pub type DirProvider = fn() -> Option<PathBuf>;

/// `~/.local/bin` — the conventional Claude Code / npm-less install location.
/// Always tried first by `discover`, so it does NOT need to appear in a spec's
/// `extra_dirs`.
pub fn local_bin_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".local").join("bin"))
}

/// Windows npm global prefix `%APPDATA%\npm` — the default target of
/// `npm i -g <pkg>` on Windows. Returns `None` off Windows / without `%APPDATA%`.
pub fn windows_npm_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("npm"))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Resolve a CLI binary from a per-OS list of candidate names plus extra search
/// directories. See the module docs for the lookup order. Returns the first
/// existing file found, or `None` if nothing matches.
///
/// This is exactly the original `claude_bin()` algorithm, parameterized on the
/// name list and the extra directories, with an added `which::which` fallback.
pub fn discover(bin_names: &[&str], extra_dirs: &[DirProvider]) -> Option<PathBuf> {
    // 1. Conventional `~/.local/bin` install location.
    if let Some(dir) = local_bin_dir() {
        if let Some(p) = first_in_dir(&dir, bin_names) {
            return Some(p);
        }
    }

    // 2. Caller-supplied extra dirs (e.g. Windows npm global prefix), in order.
    for provider in extra_dirs {
        if let Some(dir) = provider() {
            if let Some(p) = first_in_dir(&dir, bin_names) {
                return Some(p);
            }
        }
    }

    // 3. Manual `$PATH` scan (no shell), trying each candidate name so Windows
    //    executable extensions resolve.
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(p) = first_in_dir(&dir, bin_names) {
                return Some(p);
            }
        }
    }

    // 4. Last resort: let `which` resolve each name with its own platform rules.
    for name in bin_names {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }

    None
}

/// First candidate name that exists as a file directly inside `dir`.
fn first_in_dir(dir: &std::path::Path, bin_names: &[&str]) -> Option<PathBuf> {
    for name in bin_names {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(windows)]
    fn make_executable(_p: &std::path::Path) {}
    #[cfg(not(windows))]
    fn make_executable(p: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(p, perm).unwrap();
    }

    #[test]
    fn discover_finds_binary_in_extra_dir() {
        let tmp = std::env::temp_dir().join(format!("cli_disc_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let bin = tmp.join("mycli");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        make_executable(&bin);

        // Stash the dir in a thread-local-ish static via a leaked path isn't
        // possible with `fn` providers, so test through PATH scanning instead:
        // prepend our tmp dir to PATH and confirm discovery picks it up.
        let prev = std::env::var_os("PATH");
        let mut paths = vec![tmp.clone()];
        if let Some(p) = &prev {
            paths.extend(std::env::split_paths(p));
        }
        let joined = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", &joined);

        let found = discover(&["mycli"], &[]);

        // restore PATH before asserting
        match prev {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert_eq!(found.as_deref(), Some(bin.as_path()));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_returns_none_for_unknown_binary() {
        // A name that won't exist anywhere on a sane test host.
        let found = discover(&["definitely-not-a-real-cli-xyzzy-42"], &[]);
        assert!(found.is_none());
    }

    #[test]
    fn first_in_dir_respects_name_order() {
        let tmp = std::env::temp_dir().join(format!("cli_order_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let second = tmp.join("second");
        fs::write(&second, b"x").unwrap();
        // "first" doesn't exist, so the second name should win.
        let got = first_in_dir(&tmp, &["first", "second"]);
        assert_eq!(got.as_deref(), Some(second.as_path()));
        let _ = fs::remove_dir_all(&tmp);
    }
}
