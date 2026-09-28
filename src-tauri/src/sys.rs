//! Cross-platform process spawning helpers.
//!
//! On Windows, plain `std::process::Command::new(...)` makes child
//! processes that own their own console — every shell-out flashes a black
//! console window on the user's desktop. The fix is `CREATE_NO_WINDOW`
//! (0x08000000) wired via `std::os::windows::process::CommandExt::creation_flags`.
//!
//! Use [`no_window`] in place of `Command::new` for any subprocess that
//! doesn't need a visible console. On non-Windows targets it's a no-op
//! pass-through so the same code compiles cleanly. [`tokio_no_window`] is the
//! same thing for `tokio::process::Command`.
//!
//! See `feedback_no_windows_popups.md` in the project notes for the
//! original incident report.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Build a `Command` that won't pop a console window on Windows.
pub fn no_window<S: AsRef<OsStr>>(program: S) -> Command {
    // Wave 198 — `mut` only needed on Windows where we call
    // `creation_flags`. Silences the cross-platform build warning.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// [`no_window`] for `tokio::process::Command` (async children). tokio's
/// `Command` exposes `creation_flags` as an inherent method on Windows, so no
/// trait import is needed.
pub fn tokio_no_window<S: AsRef<OsStr>>(program: S) -> tokio::process::Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Resolve a bare program name (`npm`, `pnpm`, `pytest`, …) to the executable
/// `CreateProcess`/`execvp` will actually find.
///
/// Windows is the reason this exists: `Command::new("npm")` only appends
/// `.exe` when searching `PATH`, so npm/pnpm/yarn (installed as `npm.cmd` /
/// `pnpm.cmd` shims) fail with "program not found" even though typing `npm`
/// in a terminal works — the shell consults `PATHEXT`, `CreateProcess` does
/// not. `which` applies `PATHEXT`, so the `.cmd`/`.bat` shim is found and can
/// be handed straight to `Command::new` (std ≥ 1.77.2 runs `.cmd`/`.bat`
/// through `cmd.exe` with safe argument escaping). Names that already carry
/// a path separator, or that `which` can't find, are returned unchanged so
/// the spawn error stays the honest "not found".
pub fn resolve_program(name: &str) -> PathBuf {
    let has_sep = name.contains('/') || name.contains('\\');
    if has_sep {
        return PathBuf::from(name);
    }
    which::which(name).unwrap_or_else(|_| PathBuf::from(name))
}

/// Best-effort kill of a whole process TREE rooted at `pid` — the child plus
/// every descendant it spawned (an agent CLI's shell tools, `npm test`'s
/// workers, …). `Child::kill` / `kill_on_drop` only terminate the direct
/// child, which on both platforms orphans the grandchildren.
///
/// * Windows: `taskkill /T /F /PID <pid>` walks the parent→child tree. The
///   root must still be alive for the walk to work, so call this BEFORE
///   killing the child yourself.
/// * Unix: sends `SIGKILL` to the process GROUP `pid` (i.e. `kill -- -pid`),
///   which only does anything useful when the child was spawned as a group
///   leader (`Command::process_group(0)`); otherwise the negative pid names
///   the parent's own group and the signal is refused / a no-op for us.
///
/// Fire-and-forget (no wait, errors ignored) so it is safe to call from a
/// `Drop` impl or an aborted task.
pub fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = no_window("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(unix)]
    {
        let _ = no_window("kill")
            .args(["-s", "KILL", "--", &format!("-{pid}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_program_keeps_paths_and_unknown_names_verbatim() {
        // A name with a separator is never looked up.
        let p = if cfg!(windows) {
            "C:\\tools\\npm.cmd"
        } else {
            "/usr/local/bin/npm"
        };
        assert_eq!(resolve_program(p), PathBuf::from(p));
        // An unknown bare name falls back to itself so the spawn error is honest.
        let bogus = "definitely-not-a-real-program-xyzzy-42";
        assert_eq!(resolve_program(bogus), PathBuf::from(bogus));
    }

    #[test]
    fn resolve_program_finds_something_on_path() {
        // Every CI host has one of these; the resolved path must be absolute.
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        let resolved = resolve_program(name);
        assert!(
            resolved.is_absolute(),
            "expected an absolute path, got {resolved:?}"
        );
    }
}
