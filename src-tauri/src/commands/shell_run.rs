//! `/run <cmd>` — user-driven shell execution from the chat composer.
//!
//! This is **explicitly a user-initiated** exec path: the user types
//! `/run pytest -k foo` and the slash-command pipeline calls us directly.
//! It is NOT a tool an agent can call. There's no approval layer because
//! the user is the one issuing the command in real time; the safety net is
//! the 30-second timeout, the 16 KiB output cap, and running inside the
//! active project root so a stray command can't wander off into `$HOME`.
//!
//! Output is captured via a tokio child's `wait_with_output()` (under a
//! `timeout` with `kill_on_drop`) rather than streamed because the consumer is
//! a single chat-message append: we want a snapshot, not a live tail. Use the
//! `monitors` subsystem if you need streaming.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Hard cap on combined stdout+stderr bytes returned to the frontend.
/// Matches the chat-message render budget — any more and the UI starts
/// stuttering on syntax highlighting.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Wall-clock timeout. After this elapses we kill the child and return
/// the captured output so far with `exit_code = None`.
pub const TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Serialize)]
pub struct ShellResult {
    /// Process exit code; `None` when the command timed out or was killed.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Wall-clock duration of the command in milliseconds.
    pub duration_ms: u64,
    /// True when the captured stream was clipped at `MAX_OUTPUT_BYTES`.
    pub truncated: bool,
    /// True when the 30s wall-clock budget elapsed and we killed the child.
    pub timed_out: bool,
}

#[derive(Debug, Deserialize)]
pub struct ShellExecArgs {
    pub cmd: String,
    #[serde(default)]
    pub project_root: Option<String>,
}

/// Truncate `s` at `cap` bytes on a UTF-8 char boundary. Returns true when
/// truncation happened so the caller can surface a `[truncated]` hint.
fn clip(s: &mut String, cap: usize) -> bool {
    if s.len() <= cap {
        return false;
    }
    let mut cut = cap;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    true
}

/// Resolve the working directory the command must run in.
///
/// The module contract is that a `/run` command stays *confined to the active
/// project root* so a stray command can't wander off into `$HOME`. That only
/// holds if we actually pin the cwd: silently falling back to the process cwd
/// (typically `$HOME`) when `project_root` is missing or not a directory would
/// defeat the safety net. So we treat both cases as hard errors instead.
fn resolve_cwd(project_root: Option<&String>) -> Result<PathBuf, String> {
    let root = project_root.ok_or_else(|| {
        "refusing to run: no project_root supplied (commands are confined to the project root)"
            .to_string()
    })?;
    let path = PathBuf::from(root);
    if !path.is_dir() {
        return Err(format!(
            "refusing to run: project_root {root:?} is not an existing directory"
        ));
    }
    Ok(path)
}

/// Run a user-typed shell command and return a snapshot of its output.
///
/// Uses `sh -c` (or `cmd /C` on Windows) so familiar shell quoting works.
/// The working directory is pinned to `project_root`; a missing or invalid
/// root is rejected rather than silently falling back to the process cwd
/// (typically `$HOME`), which would break this module's confinement contract.
#[tauri::command]
pub async fn shell_exec(args: ShellExecArgs) -> Result<ShellResult, String> {
    let cmd = args.cmd.trim().to_string();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }

    let cwd = resolve_cwd(args.project_root.as_ref())?;

    // Build a tokio child with `kill_on_drop` so the timeout path actually
    // kills the process (the old worker-thread + `recv_timeout` design left
    // the child running and the thread blocked on `.output()` until the child
    // exited on its own — leaking a process + thread per timed-out command,
    // contradicting this module's "we kill the child" contract). When the
    // `timeout` future below elapses, `wait_with_output` is dropped, which
    // drops the child handle and triggers the SIGKILL.
    let mut command = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", &cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &cmd]);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW — suppress the flashing console window.
        command.creation_flags(0x0800_0000);
    }
    command.current_dir(cwd);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let start = Instant::now();
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };
    let output_result =
        tokio::time::timeout(Duration::from_millis(TIMEOUT_MS), child.wait_with_output()).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match output_result {
        Ok(Ok(out)) => {
            let mut stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let t1 = clip(&mut stdout, MAX_OUTPUT_BYTES);
            let t2 = clip(&mut stderr, MAX_OUTPUT_BYTES);
            Ok(ShellResult {
                exit_code: out.status.code(),
                stdout,
                stderr,
                duration_ms,
                truncated: t1 || t2,
                timed_out: false,
            })
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => Ok(ShellResult {
            exit_code: None,
            stdout: String::new(),
            stderr: format!("[shell_exec] timed out after {}ms", TIMEOUT_MS),
            duration_ms,
            truncated: false,
            timed_out: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory guaranteed to exist, used as a valid `project_root` in tests.
    fn tmp_root() -> String {
        std::env::temp_dir().to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn echoes_stdout() {
        let r = shell_exec(ShellExecArgs {
            cmd: "echo hello".to_string(),
            project_root: Some(tmp_root()),
        })
        .await
        .unwrap();
        assert_eq!(r.exit_code, Some(0));
        assert!(r.stdout.contains("hello"));
        assert!(!r.timed_out);
    }

    #[tokio::test]
    async fn empty_command_rejected() {
        let err = shell_exec(ShellExecArgs {
            cmd: "   ".to_string(),
            project_root: Some(tmp_root()),
        })
        .await
        .unwrap_err();
        assert!(err.contains("empty"));
    }

    #[tokio::test]
    async fn captures_nonzero_exit() {
        let r = shell_exec(ShellExecArgs {
            cmd: "sh -c 'exit 7'".to_string(),
            project_root: Some(tmp_root()),
        })
        .await
        .unwrap();
        assert_eq!(r.exit_code, Some(7));
    }

    #[tokio::test]
    async fn missing_project_root_rejected() {
        let err = shell_exec(ShellExecArgs {
            cmd: "echo hi".to_string(),
            project_root: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("project_root"));
    }

    #[tokio::test]
    async fn nonexistent_project_root_rejected() {
        let err = shell_exec(ShellExecArgs {
            cmd: "echo hi".to_string(),
            project_root: Some("/no/such/dir/cortex-test-xyz".to_string()),
        })
        .await
        .unwrap_err();
        assert!(err.contains("not an existing directory"));
    }

    #[test]
    fn clip_truncates_on_boundary() {
        let mut s = "héllo world".to_string();
        let truncated = clip(&mut s, 4);
        assert!(truncated);
        // 'h' = 1 byte, 'é' = 2 bytes — so cap=4 should keep 'hé' + 'l' = 4 bytes
        assert!(s.is_char_boundary(s.len()));
        assert!(s.len() <= 4);
    }
}
