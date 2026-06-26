//! Qwen Code CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against the QwenLM/qwen-code headless docs + npm
//! package `@qwen-code/qwen-code` (a Gemini-CLI fork, so the headless surface
//! matches Gemini's):
//!   * binary: `qwen`
//!   * non-interactive: `qwen -p "<prompt>"` / `--prompt`
//!   * model: `-m <model>` / `--model <model>`
//!   * structured output: `--output-format json` → a single
//!     `{ "response": ..., "stats": ... }` object ([`OutputKind::GeminiJson`])
//!   * login: `qwen auth qwen-oauth` (browser OAuth to qwen.ai), or a
//!     configured provider key. NOTE: the Qwen OAuth free tier ended
//!     2026-04-15; users may need an Alibaba Cloud / OpenRouter / Fireworks key.
//!
//! Capabilities are honest: Qwen Code reads/edits files and runs shell in
//! headless mode (same agent core as Gemini CLI).

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const QWEN_NAMES: &[&str] = &["qwen.exe", "qwen.cmd", "qwen.bat", "qwen"];
#[cfg(not(windows))]
const QWEN_NAMES: &[&str] = &["qwen"];

const QWEN_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `qwen -p <prompt> [-m <model>] --output-format json`.
fn qwen_args(ctx: &LaunchCtx) -> Vec<String> {
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

pub static QWEN_SPEC: CliSpec = CliSpec {
    id: "qwen-cli",
    label: "Qwen Code (CLI)",
    description:
        "Local Qwen Code CLI (`qwen -p`) spawned directly — your Qwen/provider login.",
    bin_names: QWEN_NAMES,
    extra_dirs: QWEN_EXTRA_DIRS,
    headless_args: qwen_args,
    output_kind: OutputKind::GeminiJson,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::LongContext,
    ],
    install_url: "https://github.com/QwenLM/qwen-code",
    install_hint:
        "Install Qwen Code (`npm i -g @qwen-code/qwen-code`) and run `qwen auth qwen-oauth`.",
    tag: "qwen",
    login_cmd: &["qwen", "auth", "qwen-oauth"],
    default_model: "",
    model_prefixes: &["qwen"],
    auth_paths: &[".qwen/oauth_creds.json"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    #[test]
    fn descriptor_is_stable() {
        let d = GenericCliAgent::new(&QWEN_SPEC).descriptor();
        assert_eq!(d.id, "qwen-cli");
        assert_eq!(QWEN_SPEC.output_kind, OutputKind::GeminiJson);
    }
}
