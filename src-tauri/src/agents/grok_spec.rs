//! xAI Grok Build CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against xAI's Grok Build announcement + setup guides:
//!   * binary: `grok` (xAI's first-party "Grok Build", NOT the community
//!     `superagent-ai/grok-cli` wrapper)
//!   * install: `curl -fsSL https://x.ai/cli/install.sh | bash`
//!   * non-interactive: `grok -p "<prompt>"`
//!   * model: `--model <model>` (precedence over `GROK_MODEL` / settings)
//!   * structured output: `--output-format streaming-json` → newline-delimited
//!     thread/turn/item events, same vocabulary as Codex
//!     ([`OutputKind::CodexJsonl`])
//!   * login: `grok login` (browser OAuth; `XAI_API_KEY` for headless servers)
//!   * `--always-approve` so the headless run isn't blocked on tool approvals.
//!
//! FLAG CONFIDENCE: the `-p`, `--model`, `--output-format streaming-json`,
//! `--always-approve` and `grok login` surface is taken from xAI's launch
//! guides (May 2026). Grok Build is a fast-moving early beta — NOTE: confirm
//! the exact `--output-format` value + event schema against your installed
//! `grok` if the stream comes through empty.

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const GROK_NAMES: &[&str] = &["grok.exe", "grok.cmd", "grok.bat", "grok"];
#[cfg(not(windows))]
const GROK_NAMES: &[&str] = &["grok"];

const GROK_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `grok -p <prompt> [--model <model>] --output-format streaming-json
/// --always-approve`.
fn grok_args(ctx: &LaunchCtx) -> Vec<String> {
    let mut args = vec!["-p".into(), ctx.prompt.to_string()];
    let model = ctx.model.trim();
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    args.push("--output-format".into());
    args.push("streaming-json".into());
    args.push("--always-approve".into());
    args
}

pub static GROK_SPEC: CliSpec = CliSpec {
    id: "grok-cli",
    label: "Grok Build (CLI)",
    description:
        "Local xAI Grok Build CLI (`grok -p`) spawned directly — your xAI/SuperGrok login.",
    bin_names: GROK_NAMES,
    extra_dirs: GROK_EXTRA_DIRS,
    headless_args: grok_args,
    output_kind: OutputKind::CodexJsonl,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::LongContext,
        AgentCapability::Approval,
    ],
    install_url: "https://x.ai/news/grok-build-cli",
    install_hint:
        "Install Grok Build (`curl -fsSL https://x.ai/cli/install.sh | bash`) and run `grok login`.",
    tag: "grok",
    login_cmd: &["grok", "login"],
    default_model: "",
    model_prefixes: &["grok"],
    // Auth-file location for Grok Build isn't pinned in the public docs; these
    // are best-guess. If neither exists we just report `authenticated: None`.
    auth_paths: &[".grok/auth.json", ".config/grok/auth.json"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatRequest;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    #[test]
    fn descriptor_is_stable() {
        let d = GenericCliAgent::new(&GROK_SPEC).descriptor();
        assert_eq!(d.id, "grok-cli");
        assert_eq!(GROK_SPEC.output_kind, OutputKind::CodexJsonl);
    }

    #[test]
    fn args_shape() {
        let r = ChatRequest {
            session_id: "s".into(),
            message: "hi".into(),
            project_root: None,
            history: vec![],
            model: None,
            reasoning_effort: None,
        };
        let args = (GROK_SPEC.headless_args)(&LaunchCtx {
            prompt: "fix it",
            model: "grok-build-0.1",
            req: &r,
        });
        assert_eq!(
            args,
            vec![
                "-p",
                "fix it",
                "--model",
                "grok-build-0.1",
                "--output-format",
                "streaming-json",
                "--always-approve",
            ]
        );
    }
}
