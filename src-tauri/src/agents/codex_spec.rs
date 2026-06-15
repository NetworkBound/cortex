//! OpenAI Codex CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against the OpenAI Codex docs:
//!   * binary: `codex`
//!   * non-interactive: `codex exec <prompt>` (read-only sandbox by default;
//!     we pass `--sandbox workspace-write` so it can actually edit the project)
//!   * machine-readable stream: `--json` → newline-delimited
//!     `thread.*`/`turn.*`/`item.*`/`error` events (parsed by [`OutputKind::CodexJsonl`])
//!   * model: `-m <model>` / `--model <model>`
//!   * login: `codex login` (browser ChatGPT OAuth)
//!
//! `--skip-git-repo-check` keeps `codex exec` from refusing to run outside a git
//! repo (Cortex projects aren't always git repos). Capabilities are honest:
//! `codex exec` with `workspace-write` truly edits files and runs shell.

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const CODEX_NAMES: &[&str] = &["codex.exe", "codex.cmd", "codex.bat", "codex"];
#[cfg(not(windows))]
const CODEX_NAMES: &[&str] = &["codex"];

const CODEX_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `codex exec --json --skip-git-repo-check --sandbox workspace-write
/// [-m <model>] -- <prompt>`. The model flag is omitted when no slug is
/// resolved, so Codex uses the user's own configured default.
fn codex_args(ctx: &LaunchCtx) -> Vec<String> {
    let mut args = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--sandbox".into(),
        "workspace-write".into(),
    ];
    let model = ctx.model.trim();
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    // `--` terminates flag parsing so a prompt that begins with `-` is safe.
    args.push("--".into());
    args.push(ctx.prompt.to_string());
    args
}

pub static CODEX_SPEC: CliSpec = CliSpec {
    id: "codex-cli",
    label: "OpenAI Codex (CLI)",
    description:
        "Local OpenAI Codex CLI (`codex exec`) spawned directly — your ChatGPT/Codex login.",
    bin_names: CODEX_NAMES,
    extra_dirs: CODEX_EXTRA_DIRS,
    headless_args: codex_args,
    output_kind: OutputKind::CodexJsonl,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::LongContext,
        AgentCapability::Approval,
    ],
    install_url: "https://developers.openai.com/codex/cli",
    install_hint: "Install the Codex CLI (`npm i -g @openai/codex`) and run `codex login`.",
    tag: "codex",
    login_cmd: &["codex", "login"],
    // No Cortex-curated default: let Codex use the account's own default model
    // unless the user explicitly typed a gpt/o-series/codex slug.
    default_model: "",
    model_prefixes: &["gpt-", "gpt5", "gpt4", "o1", "o3", "o4", "codex"],
    auth_paths: &[".codex/auth.json"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatRequest;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    fn ctx<'a>(prompt: &'a str, model: &'a str, req: &'a ChatRequest) -> LaunchCtx<'a> {
        LaunchCtx { prompt, model, req }
    }

    #[test]
    fn descriptor_is_stable() {
        let d = GenericCliAgent::new(&CODEX_SPEC).descriptor();
        assert_eq!(d.id, "codex-cli");
        assert!(d.capabilities.contains(&AgentCapability::ShellExec));
    }

    #[test]
    fn args_with_model() {
        let r = ChatRequest {
            session_id: "s".into(),
            message: "hi".into(),
            project_root: None,
            history: vec![],
            model: None,
            reasoning_effort: None,
        };
        let args = (CODEX_SPEC.headless_args)(&ctx("do it", "gpt-5.5", &r));
        assert_eq!(
            args,
            vec![
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "-m",
                "gpt-5.5",
                "--",
                "do it",
            ]
        );
    }

    #[test]
    fn args_without_model_omit_flag() {
        let r = ChatRequest {
            session_id: "s".into(),
            message: "hi".into(),
            project_root: None,
            history: vec![],
            model: None,
            reasoning_effort: None,
        };
        let args = (CODEX_SPEC.headless_args)(&ctx("do it", "", &r));
        assert!(!args.iter().any(|a| a == "-m"));
        assert_eq!(args.last().unwrap(), "do it");
    }
}
