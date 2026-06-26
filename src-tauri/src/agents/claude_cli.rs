//! Local Claude Code CLI discovery.
//!
//! The Claude adapter itself is now data-driven: it's expressed as
//! [`crate::agents::claude_spec::CLAUDE_SPEC`] and run by the generic
//! [`crate::agents::local_cli::GenericCliAgent`]. What remains here is the
//! `claude_bin()` resolver, kept as a stable entry point for the few callers
//! that only need to know whether the `claude` binary is installed
//! (`commands::settings`, `commands::models`).

use std::path::PathBuf;

/// Resolve the `claude` binary, or `None` if it isn't installed. Delegates to
/// the shared, data-driven [`crate::agents::cli_discovery::discover`] with
/// Claude's per-OS candidate names: `~/.local/bin` → Windows npm prefix →
/// `$PATH` scan → `which`. Behavior is unchanged from the original hand-rolled
/// lookup.
pub fn claude_bin() -> Option<PathBuf> {
    #[cfg(windows)]
    const NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude.bat", "claude"];
    #[cfg(not(windows))]
    const NAMES: &[&str] = &["claude"];

    super::cli_discovery::discover(NAMES, &[super::cli_discovery::windows_npm_dir])
}
