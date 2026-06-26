//! Claude expressed as a [`CliSpec`] for the generic local-CLI framework.
//!
//! Registering `GenericCliAgent::new(&CLAUDE_SPEC)` produces an adapter that is
//! byte-for-byte equivalent to the original hand-written `ClaudeCliAgent`: same
//! registry id (`"claude-cli"`), same capabilities, same headless invocation
//! (`claude -p <prompt> --output-format stream-json --include-partial-messages
//! --verbose --model <slug>`), and the same `stream-json` event translation.

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

/// Fallback model when the request carries no Claude-looking slug. Mirrors the
/// original `claude_cli::DEFAULT_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Per-OS candidate binary names for the `claude` CLI. Windows installs are
/// `.exe` (native), `.cmd` (npm shim), or `.bat`; POSIX is the bare name.
#[cfg(windows)]
const CLAUDE_NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude.bat", "claude"];
#[cfg(not(windows))]
const CLAUDE_NAMES: &[&str] = &["claude"];

/// Extra search dirs: the Windows npm global prefix (`%APPDATA%\npm`). Resolves
/// to nothing off Windows. `~/.local/bin` and `$PATH` are always searched by
/// `discover`, so they don't appear here.
const CLAUDE_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// Build Claude's headless argv (excluding the binary). Exactly the original
/// invocation: `-p <prompt> --output-format stream-json --include-partial-messages
/// --verbose --model <slug>`.
fn claude_args(ctx: &LaunchCtx) -> Vec<String> {
    vec![
        "-p".into(),
        ctx.prompt.to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--include-partial-messages".into(),
        "--verbose".into(),
        "--model".into(),
        ctx.model.to_string(),
    ]
}

/// The Claude CLI spec. Same id/label/capabilities/args as the original adapter.
pub static CLAUDE_SPEC: CliSpec = CliSpec {
    id: "claude-cli",
    label: "Claude (CLI)",
    description:
        "Local Claude Code CLI (`claude`) spawned directly — bypasses the Cortex Gateway.",
    bin_names: CLAUDE_NAMES,
    extra_dirs: CLAUDE_EXTRA_DIRS,
    headless_args: claude_args,
    output_kind: OutputKind::ClaudeStreamJson,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::Vision,
        AgentCapability::LongContext,
        AgentCapability::Approval,
    ],
    install_url: "https://docs.anthropic.com/en/docs/claude-code",
    install_hint:
        "Install Claude Code (expected at ~/.local/bin/claude or on PATH).",
    tag: "claude",
    // `claude /login` opens the Anthropic OAuth flow.
    login_cmd: &["claude", "/login"],
    default_model: DEFAULT_MODEL,
    model_prefixes: &["claude", "opus", "sonnet", "haiku"],
    auth_paths: &[".claude/.credentials.json", ".claude.json"],
};
