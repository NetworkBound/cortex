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
use serde_json::Value;

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

/// Resume Claude Code's own session `native_id` with `ctx.prompt` as the ONLY
/// new user message: `-p <msg> --resume <id> --output-format stream-json
/// --include-partial-messages --verbose --model <slug>`. Documented headless
/// form: `claude -p --resume <session-id> "query"` — "you only send the new
/// message". The other flags mirror [`claude_args`] so the event stream
/// Cortex parses is identical.
fn claude_resume_args(ctx: &LaunchCtx, native_id: &str) -> Vec<String> {
    vec![
        "-p".into(),
        ctx.prompt.to_string(),
        "--resume".into(),
        native_id.to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--include-partial-messages".into(),
        "--verbose".into(),
        "--model".into(),
        ctx.model.to_string(),
    ]
}

/// Claude's `stream-json` carries the session id on the first event
/// (`{"type":"system","subtype":"init","session_id":"…"}`) and again on the
/// terminal `result`. Pure; any other event → `None`.
pub(crate) fn claude_native_session_id(json: &Value) -> Option<String> {
    let ty = json.get("type").and_then(Value::as_str)?;
    let carries_id = match ty {
        "system" => json.get("subtype").and_then(Value::as_str) == Some("init"),
        "result" => true,
        _ => false,
    };
    if !carries_id {
        return None;
    }
    json.get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The Claude CLI spec. Same id/label/capabilities/args as the original adapter.
pub static CLAUDE_SPEC: CliSpec = CliSpec {
    id: "claude-cli",
    label: "Claude (CLI)",
    description: "Local Claude Code CLI (`claude`) spawned directly — bypasses the Cortex Gateway.",
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
    install_hint: "Install Claude Code (expected at ~/.local/bin/claude or on PATH).",
    tag: "claude",
    // `claude /login` opens the Anthropic OAuth flow.
    login_cmd: &["claude", "/login"],
    default_model: DEFAULT_MODEL,
    model_prefixes: &["claude", "opus", "sonnet", "haiku"],
    auth_paths: &[".claude/.credentials.json", ".claude.json"],
    // Native session continuity (`--resume`): ON by default for Claude — the
    // headless resume flow is documented and the fold fallback covers a
    // stale/missing session. `CORTEX_CLI_NATIVE_RESUME=0` turns it off.
    resume_args: Some(claude_resume_args),
    native_session_id: Some(claude_native_session_id),
    resume_default_on: true,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatRequest;

    fn req() -> ChatRequest {
        ChatRequest {
            session_id: "s".into(),
            message: "next question".into(),
            project_root: None,
            history: vec![],
            model: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn resume_argv_sends_only_the_new_message() {
        let r = req();
        let ctx = LaunchCtx {
            prompt: "next question",
            model: "claude-sonnet-4-6",
            req: &r,
        };
        let args = (CLAUDE_SPEC.resume_args.expect("claude supports resume"))(
            &ctx,
            "9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d",
        );
        assert_eq!(
            args,
            vec![
                "-p",
                "next question",
                "--resume",
                "9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--model",
                "claude-sonnet-4-6",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("<conversation_history>")));
    }

    #[test]
    fn parses_session_id_from_init_and_result_only() {
        let init: Value = serde_json::from_str(
            r#"{"type":"system","subtype":"init","cwd":"/p","session_id":"abc-123-def-456","tools":[],"model":"claude-sonnet-4-6"}"#,
        )
        .unwrap();
        assert_eq!(
            claude_native_session_id(&init).as_deref(),
            Some("abc-123-def-456")
        );
        let result: Value = serde_json::from_str(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"abc-123-def-456"}"#,
        )
        .unwrap();
        assert_eq!(
            claude_native_session_id(&result).as_deref(),
            Some("abc-123-def-456")
        );
        // Other system subtypes, stream events and rate-limit events carry none.
        for raw in [
            r#"{"type":"system","subtype":"compact_boundary","session_id":"zzz"}"#,
            r#"{"type":"stream_event","session_id":"zzz","event":{"type":"message_start"}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
            r#"{"type":"system","subtype":"init","session_id":""}"#,
        ] {
            let v: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(claude_native_session_id(&v), None, "{raw}");
        }
    }
}
