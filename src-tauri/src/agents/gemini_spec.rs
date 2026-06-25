//! Google Gemini CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against the Gemini CLI headless docs + npm package
//! `@google/gemini-cli`:
//!   * binary: `gemini`
//!   * non-interactive: `gemini -p "<prompt>"` (headless when `-p`/`--prompt`
//!     is given or stdin is non-TTY)
//!   * model: `-m <model>` / `--model <model>`
//!   * structured output: `--output-format json` → a single
//!     `{ "response": ..., "stats": ..., "error"? }` object
//!     (parsed by [`OutputKind::GeminiJson`])
//!   * login: interactive ("Login with Google" on first run) — there is no
//!     headless `gemini login` subcommand, so the Sign-in button just launches
//!     the bare `gemini` binary in a PTY and the user picks "Login with Google".
//!
//! Capabilities are honest: Gemini CLI is an agent that reads/edits files and
//! runs shell tools in non-interactive mode (it auto-approves in headless mode).

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const GEMINI_NAMES: &[&str] = &["gemini.exe", "gemini.cmd", "gemini.bat", "gemini"];
#[cfg(not(windows))]
const GEMINI_NAMES: &[&str] = &["gemini"];

const GEMINI_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `gemini -p <prompt> [-m <model>] --output-format json`.
fn gemini_args(ctx: &LaunchCtx) -> Vec<String> {
    let mut args = vec!["-p".into(), ctx.prompt.to_string()];
    let model = ctx.model.trim();
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    args.push("--output-format".into());
    args.push("json".into());
    args
}

pub static GEMINI_SPEC: CliSpec = CliSpec {
    id: "gemini-cli",
    label: "Gemini (CLI)",
    description:
        "Local Google Gemini CLI (`gemini -p`) spawned directly — your Google account login.",
    bin_names: GEMINI_NAMES,
    extra_dirs: GEMINI_EXTRA_DIRS,
    headless_args: gemini_args,
    output_kind: OutputKind::GeminiJson,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::LongContext,
    ],
    install_url: "https://github.com/google-gemini/gemini-cli",
    install_hint:
        "Install the Gemini CLI (`npm i -g @google/gemini-cli`) and sign in with Google.",
    tag: "gemini",
    // No headless login subcommand — launch the bare binary so the user can
    // pick "Login with Google".
    login_cmd: &["gemini"],
    default_model: "",
    model_prefixes: &["gemini"],
    auth_paths: &[".gemini/oauth_creds.json", ".gemini/google_accounts.json"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatRequest;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    #[test]
    fn descriptor_is_stable() {
        let d = GenericCliAgent::new(&GEMINI_SPEC).descriptor();
        assert_eq!(d.id, "gemini-cli");
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
        let args = (GEMINI_SPEC.headless_args)(&LaunchCtx {
            prompt: "summarize",
            model: "gemini-3.1-pro-preview",
            req: &r,
        });
        assert_eq!(
            args,
            vec![
                "-p",
                "summarize",
                "-m",
                "gemini-3.1-pro-preview",
                "--output-format",
                "json",
            ]
        );
    }
}
