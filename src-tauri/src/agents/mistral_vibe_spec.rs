//! Mistral Vibe CLI expressed as a [`CliSpec`].
//!
//! Verified (June 2026) against Mistral's Vibe CLI docs + npm package
//! `@mistralai/vibe-cli`:
//!   * binary: `vibe`
//!   * non-interactive: `vibe --prompt "<task>"` (programmatic mode; does not
//!     start the chat UI; runs the `auto-approve` agent by default)
//!   * unattended posture: `--agent auto-approve`, `--max-turns N`
//!   * structured output: `--output text|json|streaming` — we take `text`
//!     ([`OutputKind::PlainTextStream`]) since the JSON event schema isn't
//!     pinned down in the public docs yet.
//!   * model: configured via `vibe config set model <devstral-…>` /
//!     `MISTRAL_API_KEY`; there is no robust per-invocation `--model` flag
//!     documented, so we do NOT pass one (let the user's config win).
//!   * login: `vibe --setup` (prompts for the Mistral API key); env
//!     `MISTRAL_API_KEY` also works.
//!
//! FLAG CONFIDENCE: the `--prompt`, `--agent auto-approve`, `--max-turns`,
//! `--output text` surface is from Mistral's Vibe CLI docs (Dec 2025 launch).
//! NOTE: confirm `--output text` prints a clean final answer on your
//! installed `vibe`; if it interleaves tool chatter, consider `--output json`
//! once that schema is documented.
//!
//! Capability honesty: Vibe edits files and (with auto-approve) runs tools
//! including shell, so CodeEdit + ShellExec are advertised.

use super::adapter::AgentCapability;
use super::cli_discovery::{self, DirProvider};
use super::local_cli::{CliSpec, LaunchCtx, OutputKind};

#[cfg(windows)]
const VIBE_NAMES: &[&str] = &["vibe.exe", "vibe.cmd", "vibe.bat", "vibe"];
#[cfg(not(windows))]
const VIBE_NAMES: &[&str] = &["vibe"];

const VIBE_EXTRA_DIRS: &[DirProvider] = &[cli_discovery::windows_npm_dir];

/// `vibe --prompt <task> --agent auto-approve --max-turns 25 --output text`.
fn vibe_args(ctx: &LaunchCtx) -> Vec<String> {
    vec![
        "--prompt".into(),
        ctx.prompt.to_string(),
        "--agent".into(),
        "auto-approve".into(),
        "--max-turns".into(),
        "25".into(),
        "--output".into(),
        "text".into(),
    ]
}

pub static MISTRAL_VIBE_SPEC: CliSpec = CliSpec {
    id: "mistral-vibe-cli",
    label: "Mistral Vibe (CLI)",
    description:
        "Local Mistral Vibe CLI (`vibe --prompt`) spawned directly — your Mistral API key.",
    bin_names: VIBE_NAMES,
    extra_dirs: VIBE_EXTRA_DIRS,
    headless_args: vibe_args,
    output_kind: OutputKind::PlainTextStream,
    capabilities: &[
        AgentCapability::Chat,
        AgentCapability::CodeEdit,
        AgentCapability::ShellExec,
        AgentCapability::LongContext,
    ],
    install_url: "https://docs.mistral.ai/mistral-vibe/introduction",
    install_hint:
        "Install Mistral Vibe (`npm i -g @mistralai/vibe-cli`) and run `vibe --setup`.",
    tag: "vibe",
    login_cmd: &["vibe", "--setup"],
    default_model: "",
    model_prefixes: &["devstral", "mistral", "magistral"],
    // Vibe stores config (incl. the API key) under ~/.vibe — its presence is a
    // reasonable "configured" hint.
    auth_paths: &[".vibe/config.toml", ".vibe/auth.json"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::AgentAdapter;
    use crate::agents::local_cli::GenericCliAgent;

    #[test]
    fn descriptor_is_stable() {
        let d = GenericCliAgent::new(&MISTRAL_VIBE_SPEC).descriptor();
        assert_eq!(d.id, "mistral-vibe-cli");
        assert_eq!(MISTRAL_VIBE_SPEC.output_kind, OutputKind::PlainTextStream);
    }
}
