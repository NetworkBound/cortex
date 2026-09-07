//! aider expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against aider's scripting docs:
//!   * binary: `aider`
//!   * non-interactive: `aider --message "<msg>"` (sends one message, prints the
//!     reply, exits; disables the chat UI)
//!   * unattended posture: `--yes-always` (auto-confirm), `--no-stream`,
//!     `--no-pretty` (clean machine-readable text), `--no-check-update`,
//!     `--no-analytics`
//!   * model: `--model <provider/model-id>` (LiteLLM-style; multi-provider)
//!   * login: NONE — aider authenticates purely via the provider API-key env
//!     vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …) or its `.env`. So there
//!     is no Sign-in button action; the Settings hint points at key setup.
//!   * output: plain text → [`OutputKind::PlainTextStream`].
//!
//! Capability honesty: aider edits files (CodeEdit) and confirms via `--yes`,
//! but it does NOT autonomously run shell commands in this posture (its shell /
//! `/run` is interactive-opt-in), so `ShellExec` is intentionally NOT
//! advertised — cost_router will never route a shell-requiring task here.

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const AIDER_NAMES: &[&str] = &["aider.exe", "aider.cmd", "aider.bat", "aider"];
#[cfg(not(windows))]
const AIDER_NAMES: &[&str] = &["aider"];

// aider is typically a pipx/pip install, so it lands on PATH or in ~/.local/bin
// (already searched by `discover`). No extra dirs needed.
const AIDER_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `aider --message <msg> --yes-always --no-stream --no-pretty --no-check-update
/// --no-analytics [--model <model>]`.
fn aider_args(ctx: &LaunchCtx) -> Vec<String> {
    let mut args = vec![
        "--message".into(),
        ctx.prompt.to_string(),
        "--yes-always".into(),
        "--no-stream".into(),
        "--no-pretty".into(),
        "--no-check-update".into(),
        "--no-analytics".into(),
    ];
    let model = ctx.model.trim();
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    args
}

pub static AIDER_SPEC: CliSpec = CliSpec {
    id: "aider-cli",
    label: "aider (CLI)",
    description:
        "Local aider CLI (`aider --message`) spawned directly — multi-provider via your API keys.",
    bin_names: AIDER_NAMES,
    extra_dirs: AIDER_EXTRA_DIRS,
    headless_args: aider_args,
    output_kind: OutputKind::PlainTextStream,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::LongContext,
    ],
    install_url: "https://aider.chat/docs/install.html",
    install_hint:
        "Install aider (`pipx install aider-chat`) and set your provider API key env var.",
    tag: "aider",
    // aider has no login flow — auth is provider API-key env vars.
    login_cmd: &[],
    default_model: "",
    // aider takes LiteLLM provider/model strings; never claim a slug, always
    // forward the raw model (or let aider use its configured default).
    model_prefixes: &[],
    // aider auths via env API keys — no auth file to probe → `authenticated:
    // None` (the UI explains keys instead of offering a sign-in flow).
    auth_paths: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatRequest;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    #[test]
    fn descriptor_omits_shellexec() {
        let d = GenericCliAgent::new(&AIDER_SPEC).descriptor();
        assert_eq!(d.id, "aider-cli");
        assert!(
            !d.capabilities.contains(&AgentCapability::ShellExec),
            "aider must not advertise ShellExec in this posture"
        );
        assert!(d.capabilities.contains(&AgentCapability::CodeEdit));
    }

    #[test]
    fn args_include_unattended_flags() {
        let r = ChatRequest {
            session_id: "s".into(),
            message: "hi".into(),
            project_root: None,
            history: vec![],
            model: None,
            reasoning_effort: None,
        };
        let args = (AIDER_SPEC.headless_args)(&LaunchCtx {
            prompt: "edit",
            model: "openrouter/anthropic/claude-sonnet-4",
            req: &r,
        });
        assert!(args.iter().any(|a| a == "--yes-always"));
        assert!(args.iter().any(|a| a == "--message"));
        assert_eq!(args.last().unwrap(), "openrouter/anthropic/claude-sonnet-4");
    }
}
