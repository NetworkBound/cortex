//! Cross-platform process spawning helpers.
//!
//! On Windows, plain `std::process::Command::new(...)` makes child
//! processes that own their own console — every shell-out flashes a black
//! console window on the user's desktop. The fix is `CREATE_NO_WINDOW`
//! (0x08000000) wired via `std::os::windows::process::CommandExt::creation_flags`.
//!
//! Use [`no_window`] in place of `Command::new` for any subprocess that
//! doesn't need a visible console. On non-Windows targets it's a no-op
//! pass-through so the same code compiles cleanly.
//!
//! See `feedback_no_windows_popups.md` in user's auto-memory for the
//! original incident report.

use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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
