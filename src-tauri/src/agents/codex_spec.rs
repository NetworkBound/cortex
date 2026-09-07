//! OpenAI Codex CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against the OpenAI Codex docs:
//!   * binary: `codex`
//!   * non-interactive: `codex exec <prompt>` — we pass `--sandbox <tier>`
//!     mapped from the project's effective Cortex sandbox tier (untrusted →
//!     `read-only`, trusted → its `.cortex/sandbox.toml` tier)
//!   * machine-readable stream: `--json` → newline-delimited
//!     `thread.*`/`turn.*`/`item.*`/`error` events (parsed by [`OutputKind::CodexJsonl`])
//!   * model: `-m <model>` / `--model <model>`
//!   * login: `codex login` (browser ChatGPT OAuth)
//!
//! `--skip-git-repo-check` keeps `codex exec` from refusing to run outside a git
//! repo (Cortex projects aren't always git repos). Capabilities are honest:
//! `codex exec` with `workspace-write` truly edits files and runs shell (in a
//! trusted project; untrusted projects pin it to `read-only`).

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const CODEX_NAMES: &[&str] = &["codex.exe", "codex.cmd", "codex.bat", "codex"];
#[cfg(not(windows))]
const CODEX_NAMES: &[&str] = &["codex"];

const CODEX_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// Map Cortex's resolved sandbox tier onto Codex CLI's `--sandbox` values
/// (verified against codex-cli 0.142.2: `read-only`, `workspace-write`,
/// `danger-full-access`).
fn tier_to_codex_sandbox(tier: crate::orchestrator::SandboxTier) -> &'static str {
    use crate::orchestrator::SandboxTier::*;
    match tier {
        ReadOnly => "read-only",
        WorkspaceWrite => "workspace-write",
        DangerFullAccess => "danger-full-access",
    }
}

/// `codex exec --json --skip-git-repo-check --sandbox <tier> [-m <model>]
/// -- <prompt>`. The model flag is omitted when no slug is resolved, so Codex
/// uses the user's own configured default.
///
/// The sandbox flag follows the project's EFFECTIVE tier (trust gate
/// included) instead of the old hardcoded `workspace-write`: Codex sandboxes
/// itself in its own subprocess before Cortex's event-loop gate can see any
/// tool call, so an untrusted project previously got write+exec access from
/// Codex no matter what Cortex's own tier said.
fn codex_args(ctx: &LaunchCtx) -> Vec<String> {
    let tier = crate::orchestrator::effective_tier(ctx.req.project_root.as_deref());
    let mut args = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--sandbox".into(),
        tier_to_codex_sandbox(tier).into(),
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
    fn args_with_model_pin_read_only_without_a_trusted_project() {
        // No project root → untrusted → the CLI must be sandboxed read-only.
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
                "read-only",
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

    #[test]
    fn untrusted_project_is_sandboxed_read_only_even_with_permissive_tier_file() {
        crate::paths::test_home::with_temp_home(|home| {
            // A project dir whose OWN sandbox.toml says danger-full-access —
            // but it was never trusted, so the tier file must be ignored.
            let project = home.join("evil-project");
            std::fs::create_dir_all(project.join(".cortex")).unwrap();
            std::fs::write(
                project.join(".cortex").join("sandbox.toml"),
                "tier = \"danger-full-access\"\n",
            )
            .unwrap();
            let r = ChatRequest {
                session_id: "s".into(),
                message: "hi".into(),
                project_root: Some(project),
                history: vec![],
                model: None,
                reasoning_effort: None,
            };
            let args = (CODEX_SPEC.headless_args)(&ctx("do it", "", &r));
            let i = args.iter().position(|a| a == "--sandbox").unwrap();
            assert_eq!(args[i + 1], "read-only");
        });
    }

    #[test]
    fn trusted_project_uses_its_configured_tier() {
        crate::paths::test_home::with_temp_home(|home| {
            let project = home.join("my-project");
            std::fs::create_dir_all(project.join(".cortex")).unwrap();
            crate::orchestrator::trust::trust_path(&project).unwrap();

            // Default (no sandbox.toml) → WorkspaceWrite.
            let r = ChatRequest {
                session_id: "s".into(),
                message: "hi".into(),
                project_root: Some(project.clone()),
                history: vec![],
                model: None,
                reasoning_effort: None,
            };
            let args = (CODEX_SPEC.headless_args)(&ctx("do it", "", &r));
            let i = args.iter().position(|a| a == "--sandbox").unwrap();
            assert_eq!(args[i + 1], "workspace-write");

            // An explicit read-only tier on a trusted project is honored too.
            std::fs::write(
                project.join(".cortex").join("sandbox.toml"),
                "tier = \"read-only\"\n",
            )
            .unwrap();
            let args = (CODEX_SPEC.headless_args)(&ctx("do it", "", &r));
            let i = args.iter().position(|a| a == "--sandbox").unwrap();
            assert_eq!(args[i + 1], "read-only");
        });
    }
}
