//! Hook execution. One async function spawns a single hook command,
//! writes the JSON payload to its stdin, and collects stdout/stderr under
//! a hard timeout. A second helper (`fire_event`) runs every hook for a
//! given event in sequence and returns the first one that blocked.
//!
//! Why sequential: hooks for the same event commonly depend on each
//! other (e.g. a logger then a gate). Concurrent execution would force
//! us to merge stdin/stdout semantics in ways the upstream Claude Code
//! spec doesn't define.

use crate::hooks::HookSpec;

use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Default timeout for any hook that doesn't set `timeout_ms`.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Hard upper bound on captured stdout/stderr per stream. Anything past
/// this is dropped with a `…[truncated]` suffix so a runaway hook can't
/// blow up memory or the event store.
pub const MAX_HOOK_OUTPUT_BYTES: usize = 100 * 1024;

const TRUNCATED_SUFFIX: &str = "\n…[truncated]";

/// Gating events whose hooks can deny an action. For these, an abnormal
/// hook exit (killed by a signal or timeout, i.e. no exit code) must fail
/// CLOSED — we treat it as a block rather than silently allowing the
/// action through. Non-gating events only log, so an abnormal exit there
/// stays non-blocking.
const GATING_EVENTS: &[&str] = &["PreToolUse", "UserPromptSubmit", "PermissionRequest"];

fn is_gating_event(event_name: &str) -> bool {
    GATING_EVENTS.contains(&event_name)
}

/// Validate a hook's command + args before we ever spawn them. The spec is
/// read verbatim from on-disk JSON, so a tampered or malformed config could
/// otherwise hand `Command::new` an empty program, a program with an embedded
/// NUL (silently truncated by the OS), or arguments carrying NUL/control
/// bytes. We exec directly (never through a shell), so shell-metacharacter
/// injection isn't in scope, but we still confine the command itself:
///
/// - reject an empty command,
/// - reject NUL and control characters in the command or any arg,
/// - require the command be a bare program name (resolved via PATH) or an
///   absolute path, never a relative path like `./evil` or `../../evil` that
///   would resolve against the process's current working directory.
///
/// Returns a human-readable reason on rejection; `Ok(())` when the spec is
/// safe to spawn.
fn validate_spec(spec: &HookSpec) -> Result<(), String> {
    let command = spec.command.trim();
    if command.is_empty() {
        return Err("hook command is empty".to_string());
    }
    if has_unsafe_chars(&spec.command) {
        return Err("hook command contains NUL or control characters".to_string());
    }
    for arg in &spec.args {
        if has_unsafe_chars(arg) {
            return Err("hook argument contains NUL or control characters".to_string());
        }
    }

    // Reject an explicit `timeout_ms: 0` rather than silently clamping it to a
    // tiny positive value (which would kill a legitimately slow hook after a
    // few ms and report a misleading "timed out"). Omit the field to get
    // `DEFAULT_TIMEOUT_MS`.
    if spec.timeout_ms == Some(0) {
        return Err("hook timeout_ms is 0; omit it for the default timeout".to_string());
    }

    // A command that looks like a path must be absolute. Bare program names
    // (no path separator) are fine — they resolve through PATH as the user
    // intends. A relative path containing `/` (or `..`) would resolve against
    // whatever cwd the app happens to have, which is not under the config
    // author's control and is the path-confinement hazard we're guarding.
    let looks_like_path = command.contains('/') || command.contains('\\');
    if looks_like_path && !std::path::Path::new(command).is_absolute() {
        return Err(format!(
            "hook command '{command}' is a relative path; use a bare program name or an absolute path"
        ));
    }

    Ok(())
}

/// True if `s` contains a NUL or any ASCII control character. These have no
/// legitimate place in a program name or argument and are a common smuggling
/// vector in tampered config.
fn has_unsafe_chars(s: &str) -> bool {
    s.chars().any(|c| c == '\0' || c.is_control())
}

/// One hook's outcome. `block == true` short-circuits the rest of the
/// chain for blocking events (PreToolUse, UserPromptSubmit,
/// PermissionRequest). Non-blocking events still run every hook but only
/// log the result.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub block: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Aggregate result returned by `fire_event`. `blocked` is `Some(result)`
/// for the first hook that returned `block == true`; otherwise `None`.
/// `results` contains every hook's outcome up to (and including) the
/// blocking one, in execution order.
#[derive(Debug, Clone, Default)]
pub struct FireResult {
    pub blocked: Option<HookResult>,
    pub results: Vec<HookResult>,
}

impl FireResult {
    pub fn is_blocked(&self) -> bool {
        self.blocked.is_some()
    }

    /// Convenience: returns the stderr of the blocking hook (if any) for
    /// surfacing in the UI as the rejection reason.
    pub fn block_reason(&self) -> Option<&str> {
        self.blocked.as_ref().map(|r| r.stderr.trim()).filter(|s| !s.is_empty())
    }
}

/// Run a single hook. Spawns `spec.command spec.args`, writes
/// `payload_json` to stdin, and waits up to `spec.timeout_ms` (or
/// `DEFAULT_TIMEOUT_MS`). Returns `HookResult { block: true, … }` when
/// the hook exits with status 2 (Claude Code spec), `block: false` for
/// any other outcome.
///
/// Failures (spawn error, timeout, killed) are surfaced as `block:
/// false` with `exit_code: -1` and a descriptive stderr so the chat loop
/// keeps running instead of dying on a misconfigured hook.
pub async fn run_hook(
    spec: &HookSpec,
    event_name: &str,
    payload_json: &str,
) -> HookResult {
    // Validate the spec (read verbatim from on-disk JSON) before spawning.
    // A rejected spec must not run; fail CLOSED for gating events so a
    // tampered/malformed config can't slip an action past a guard hook.
    if let Err(reason) = validate_spec(spec) {
        return HookResult {
            block: is_gating_event(event_name),
            stdout: String::new(),
            stderr: format!("hook '{}' rejected: {reason}", spec.command),
            exit_code: -1,
        };
    }

    let timeout_ms = spec.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).max(50);
    let timeout = Duration::from_millis(timeout_ms);

    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .env("CORTEX_HOOK_EVENT", event_name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookResult {
                block: false,
                stdout: String::new(),
                stderr: format!("hook '{}' spawn failed: {e}", spec.command),
                exit_code: -1,
            };
        }
    };

    // Stream the payload to stdin then drop the handle so the child sees
    // EOF and can exit naturally. We don't fail the hook on stdin write
    // errors — the child may legitimately ignore stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let buf = payload_json.as_bytes().to_vec();
        // Spawn the write so a child that ignores stdin (and never reads)
        // doesn't block us. Dropped on timeout via kill_on_drop.
        tokio::spawn(async move {
            let _ = stdin.write_all(&buf).await;
            let _ = stdin.shutdown().await;
        });
    }

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            // Fail CLOSED for gating events: we can't prove the hook allowed it.
            return HookResult {
                block: is_gating_event(event_name),
                stdout: String::new(),
                stderr: format!("hook '{}' wait failed: {e}", spec.command),
                exit_code: -1,
            };
        }
        Err(_) => {
            // Timeout: kill_on_drop on the spawned child handle would
            // have triggered when we exited the timeout future, but
            // wait_with_output already consumed `child`, so the kill
            // happens via the dropped tokio process — there's nothing
            // for us to clean up here.
            // Fail CLOSED for gating events: a timed-out gate hook must not
            // silently allow the action it was meant to guard.
            return HookResult {
                block: is_gating_event(event_name),
                stdout: String::new(),
                stderr: format!(
                    "hook '{}' timed out after {timeout_ms}ms",
                    spec.command
                ),
                exit_code: -1,
            };
        }
    };

    let stdout = truncate_lossy(&output.stdout);
    let mut stderr = truncate_lossy(&output.stderr);
    // `code()` is `None` when the child was terminated by a signal (e.g.
    // OOM kill, manual kill, or a timeout that raced our own timer). For
    // gating events we cannot prove the hook allowed the action, so we
    // fail CLOSED and block; for non-gating events we stay non-blocking.
    let (exit_code, block) = match output.status.code() {
        // Claude Code spec: exit code 2 == block.
        Some(code) => (code, code == 2),
        None => {
            if is_gating_event(event_name) {
                if !stderr.is_empty() {
                    stderr.push('\n');
                }
                stderr.push_str(&format!(
                    "hook '{}' terminated abnormally (no exit code); blocking gating event '{event_name}'",
                    spec.command
                ));
            }
            (-1, is_gating_event(event_name))
        }
    };

    HookResult {
        block,
        stdout,
        stderr,
        exit_code,
    }
}

/// Fire every hook configured for `event_name` against `config`,
/// stopping at the first one that blocks. The full payload is serialized
/// once and reused across hooks.
pub async fn fire_event(
    config: &crate::hooks::HooksConfig,
    event_name: &str,
    payload: &serde_json::Value,
) -> FireResult {
    let specs = config.for_event(event_name);
    if specs.is_empty() {
        return FireResult::default();
    }

    let payload_str = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());

    let mut out = FireResult::default();
    for spec in specs {
        let result = run_hook(spec, event_name, &payload_str).await;
        let blocking = result.block;
        out.results.push(result.clone());
        if blocking {
            out.blocked = Some(result);
            break;
        }
    }
    out
}

/// UTF-8-safe truncation of raw command output. Falls back to a
/// lossy-decoded String, then clips to `MAX_HOOK_OUTPUT_BYTES` on a char
/// boundary so we never split a multibyte sequence.
fn truncate_lossy(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes).into_owned();
    if s.len() <= MAX_HOOK_OUTPUT_BYTES {
        return s;
    }
    let mut end = MAX_HOOK_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut clipped = s[..end].to_string();
    clipped.push_str(TRUNCATED_SUFFIX);
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HooksConfig;

    #[tokio::test]
    async fn missing_command_does_not_block() {
        let spec = HookSpec {
            command: "/definitely/not/a/real/binary/cortex_hook_test".to_string(),
            args: vec![],
            timeout_ms: Some(500),
        };
        let r = run_hook(&spec, "PreToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("spawn failed"));
    }

    #[tokio::test]
    async fn exit_zero_is_allow() {
        // /bin/true exits 0 — should not block.
        let spec = HookSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            timeout_ms: Some(2000),
        };
        let r = run_hook(&spec, "PreToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, 0);
    }

    #[tokio::test]
    async fn exit_two_blocks() {
        let spec = HookSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "echo nope 1>&2; exit 2".into()],
            timeout_ms: Some(2000),
        };
        let r = run_hook(&spec, "PreToolUse", "{}").await;
        assert!(r.block);
        assert_eq!(r.exit_code, 2);
        assert!(r.stderr.contains("nope"));
    }

    #[tokio::test]
    async fn timeout_does_not_deadlock() {
        let spec = HookSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 5".into()],
            timeout_ms: Some(150),
        };
        // Gating event: a timed-out gate hook fails CLOSED and blocks.
        let r = run_hook(&spec, "PreToolUse", "{}").await;
        assert!(r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("timed out"));
        // Non-gating event: same timeout, but nothing to guard — no block.
        let r = run_hook(&spec, "PostToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("timed out"));
    }

    #[tokio::test]
    async fn fire_event_stops_at_first_block() {
        let mut cfg = HooksConfig::default();
        cfg.events.insert(
            "PreToolUse".into(),
            vec![
                HookSpec {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "exit 0".into()],
                    timeout_ms: Some(2000),
                },
                HookSpec {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "echo blocked 1>&2; exit 2".into()],
                    timeout_ms: Some(2000),
                },
                HookSpec {
                    // Should never run because the previous one blocked.
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "exit 0".into()],
                    timeout_ms: Some(2000),
                },
            ],
        );

        let r = fire_event(&cfg, "PreToolUse", &serde_json::json!({"k":"v"})).await;
        assert!(r.is_blocked());
        assert_eq!(r.results.len(), 2);
        assert_eq!(r.block_reason(), Some("blocked"));
    }

    #[tokio::test]
    async fn fire_event_with_no_hooks_is_noop() {
        let cfg = HooksConfig::default();
        let r = fire_event(&cfg, "PreToolUse", &serde_json::json!({})).await;
        assert!(!r.is_blocked());
        assert!(r.results.is_empty());
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let spec = HookSpec {
            command: "   ".to_string(),
            args: vec![],
            timeout_ms: Some(500),
        };
        // Non-gating event stays non-blocking but still refuses to spawn.
        let r = run_hook(&spec, "PostToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("rejected"));
    }

    #[tokio::test]
    async fn relative_path_command_is_rejected() {
        let spec = HookSpec {
            command: "./evil".to_string(),
            args: vec![],
            timeout_ms: Some(500),
        };
        let r = run_hook(&spec, "PostToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("relative path"));
    }

    #[tokio::test]
    async fn rejected_spec_blocks_gating_event() {
        let spec = HookSpec {
            command: "../../../bin/evil".to_string(),
            args: vec![],
            timeout_ms: Some(500),
        };
        // PreToolUse is a gating event: a rejected spec must fail CLOSED.
        let r = run_hook(&spec, "PreToolUse", "{}").await;
        assert!(r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("rejected"));
    }

    #[tokio::test]
    async fn nul_in_arg_is_rejected() {
        let spec = HookSpec {
            command: "/bin/sh".to_string(),
            args: vec!["bad\0arg".to_string()],
            timeout_ms: Some(500),
        };
        let r = run_hook(&spec, "PostToolUse", "{}").await;
        assert!(!r.block);
        assert_eq!(r.exit_code, -1);
        assert!(r.stderr.contains("rejected"));
    }

    #[test]
    fn bare_program_name_is_allowed() {
        let spec = HookSpec {
            command: "my-hook".to_string(),
            args: vec!["--flag".to_string()],
            timeout_ms: None,
        };
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn truncate_handles_huge_output() {
        let big = vec![b'a'; MAX_HOOK_OUTPUT_BYTES * 2];
        let s = truncate_lossy(&big);
        assert!(s.len() <= MAX_HOOK_OUTPUT_BYTES + TRUNCATED_SUFFIX.len());
        assert!(s.ends_with("[truncated]"));
    }
}
