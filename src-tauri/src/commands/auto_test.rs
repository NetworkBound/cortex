//! Aider-style **test command** — `--test-cmd` / `--auto-test`.
//!
//! Aider lets you configure a single shell command that runs your project's
//! test suite (`pytest`, `cargo test`, `npm test`, …) and can run it
//! automatically after the model edits code, feeding any failure back so the
//! agent can fix it. Cortex applies edits for tool-less models via the
//! SEARCH/REPLACE applier (`apply_edits.rs`) and checkpoints them, but it had no
//! way to *verify* an edit actually works — closing that edit→verify loop is the
//! whole point of an autonomous coding agent.
//!
//! This module owns:
//!   * the per-project test command, persisted at
//!     `<project_root>/.cortex/test-command.toml`:
//!     ```toml
//!     command = "cargo test"
//!     ```
//!   * running that command inside the project root, capturing a bounded snapshot
//!     of its output (stdout+stderr **tail** — test failures and the summary line
//!     live at the *end* of the output, so unlike `shell_run` we keep the tail),
//!     under a wall-clock timeout, and reporting pass/fail from the exit code.
//!
//! The frontend `/testcmd` (set/show) and `/runtests` slashes call these
//! commands, and the `/apply` flow auto-runs the configured command after a
//! successful apply (the `--auto-test` behavior).
//!
//! The config load/write and the run-in-dir core are pure aside from the
//! filesystem / subprocess and are unit-tested end-to-end on real temp dirs with
//! trivial shell commands.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Hard cap on combined stdout+stderr bytes returned to the frontend. Larger
/// than `shell_run`'s 16 KiB because a failing test run is exactly when you want
/// to see more, but still bounded so the chat render stays smooth. We keep the
/// **tail** (see module docs).
pub const MAX_OUTPUT_BYTES: usize = 24 * 1024;

/// Wall-clock timeout for a test run. Tests are slower than an ad-hoc `/run`, so
/// this is generous (2 min); on elapse we kill the child and report `timed_out`.
pub const TIMEOUT_MS: u64 = 120_000;

/// On-disk schema for `.cortex/test-command.toml`.
#[derive(Debug, Deserialize, Serialize, Default)]
struct TestCommandFile {
    command: Option<String>,
}

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("test-command.toml")
}

/// Load the configured test command for a project, or `None` if not set. A
/// missing file, malformed TOML, or a blank command all read as `None` so a
/// project that never opts in behaves as if the feature is off.
pub fn load_test_command(project_root: &Path) -> Option<String> {
    let path = config_path(project_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let parsed: TestCommandFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("auto_test: bad toml at {}: {e}", path.display());
            return None;
        }
    };
    parsed
        .command
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}

/// Persist (or clear) the test command for a project, creating `.cortex/` if
/// needed. A blank command **clears** the setting by removing the file, so the
/// "off" state has a single representation (no file) that `load_test_command`
/// already maps to `None`.
pub fn write_test_command(project_root: &Path, command: &str) -> anyhow::Result<()> {
    let path = config_path(project_root);
    let trimmed = command.trim();
    if trimmed.is_empty() {
        // Clear: remove the file if present; absence is the canonical "unset".
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = format!("command = {}\n", toml_escape(trimmed));
        std::fs::write(&path, body)?;
        Ok(())
    }
}

/// Render a string as a TOML basic-string literal (quotes + escapes the few
/// characters that matter), so a command containing quotes/backslashes
/// round-trips. Keeps the config valid without pulling in a serializer for one
/// field.
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Outcome of running the project's test command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunOutcome {
    /// The command that was run (as configured).
    pub command: String,
    /// Process exit code; `None` when it timed out or was killed.
    pub exit_code: Option<i32>,
    /// True when the command exited 0 — the suite passed.
    pub passed: bool,
    /// Tail of stdout (most recent `MAX_OUTPUT_BYTES`).
    pub stdout: String,
    /// Tail of stderr (most recent `MAX_OUTPUT_BYTES`).
    pub stderr: String,
    pub duration_ms: u64,
    /// True when output was clipped (we kept the tail).
    pub truncated: bool,
    /// True when the wall-clock budget elapsed and we killed the child.
    pub timed_out: bool,
}

/// Keep the **tail** of `s` within `cap` bytes (on a UTF-8 char boundary).
/// Returns true when anything was dropped. Tail, not head, because a test run's
/// failures and summary are at the end.
fn clip_tail(s: &mut String, cap: usize) -> bool {
    if s.len() <= cap {
        return false;
    }
    let mut cut = s.len() - cap;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    *s = s[cut..].to_string();
    true
}

/// Run `command` inside `cwd` with a timeout, returning a bounded snapshot. The
/// core of the feature, factored out so it's testable with trivial shell
/// commands (`true`/`false`/`echo`) on a temp dir.
async fn run_in_dir(command: &str, cwd: &Path, timeout_ms: u64) -> Result<TestRunOutcome, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty test command".to_string());
    }
    if !cwd.is_dir() {
        return Err(format!(
            "refusing to run: project root {:?} is not an existing directory",
            cwd.display()
        ));
    }

    let mut builder = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        builder.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    builder
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let start = Instant::now();
    let child = builder.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let result =
        tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(out)) => {
            let mut stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let t1 = clip_tail(&mut stdout, MAX_OUTPUT_BYTES);
            let t2 = clip_tail(&mut stderr, MAX_OUTPUT_BYTES);
            let exit_code = out.status.code();
            Ok(TestRunOutcome {
                command: cmd.to_string(),
                exit_code,
                passed: exit_code == Some(0),
                stdout,
                stderr,
                duration_ms,
                truncated: t1 || t2,
                timed_out: false,
            })
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => Ok(TestRunOutcome {
            command: cmd.to_string(),
            exit_code: None,
            passed: false,
            stdout: String::new(),
            stderr: format!("[test] timed out after {timeout_ms}ms"),
            duration_ms,
            truncated: false,
            timed_out: true,
        }),
    }
}

// ---- Tauri commands -----------------------------------------------------

/// Return the configured test command for a project, or `null` if unset.
#[tauri::command]
pub async fn get_test_command(project_root: String) -> Result<Option<String>, String> {
    let root = PathBuf::from(&project_root);
    Ok(load_test_command(&root))
}

/// Set (or clear, with a blank string) the project's test command.
#[tauri::command]
pub async fn set_test_command(project_root: String, command: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    write_test_command(&root, &command).map_err(|e| format!("write failed: {e}"))
}

/// Run the project's configured test command and return the outcome. Errors if
/// no command is configured (the frontend uses that to prompt `/testcmd`).
#[tauri::command]
pub async fn run_test_command(project_root: String) -> Result<TestRunOutcome, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let cmd = load_test_command(&root).ok_or_else(|| {
        "no test command configured — set one with /testcmd <command>".to_string()
    })?;
    run_in_dir(&cmd, &root, TIMEOUT_MS).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_then_load_round_trips() {
        let td = TempDir::new().unwrap();
        write_test_command(td.path(), "cargo test").unwrap();
        assert_eq!(load_test_command(td.path()).as_deref(), Some("cargo test"));
    }

    #[test]
    fn load_missing_is_none() {
        let td = TempDir::new().unwrap();
        assert_eq!(load_test_command(td.path()), None);
    }

    #[test]
    fn blank_command_clears_the_setting() {
        let td = TempDir::new().unwrap();
        write_test_command(td.path(), "pytest").unwrap();
        assert!(load_test_command(td.path()).is_some());
        // Blank clears.
        write_test_command(td.path(), "   ").unwrap();
        assert_eq!(load_test_command(td.path()), None);
        // Clearing an already-unset project is a no-op, not an error.
        write_test_command(td.path(), "").unwrap();
        assert_eq!(load_test_command(td.path()), None);
    }

    #[test]
    fn command_with_quotes_round_trips() {
        let td = TempDir::new().unwrap();
        let cmd = r#"pytest -k "foo and bar" --maxfail=1"#;
        write_test_command(td.path(), cmd).unwrap();
        assert_eq!(load_test_command(td.path()).as_deref(), Some(cmd));
    }

    #[test]
    fn whitespace_only_file_reads_as_none() {
        let td = TempDir::new().unwrap();
        // A file that parses but whose command is blank → None.
        std::fs::create_dir_all(td.path().join(".cortex")).unwrap();
        std::fs::write(config_path(td.path()), "command = \"   \"\n").unwrap();
        assert_eq!(load_test_command(td.path()), None);
    }

    #[tokio::test]
    async fn passing_command_reports_passed() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("exit 0", td.path(), 5_000).await.unwrap();
        assert!(r.passed);
        assert_eq!(r.exit_code, Some(0));
        assert!(!r.timed_out);
    }

    #[tokio::test]
    async fn failing_command_reports_failed_with_code() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("exit 3", td.path(), 5_000).await.unwrap();
        assert!(!r.passed);
        assert_eq!(r.exit_code, Some(3));
    }

    #[tokio::test]
    async fn captures_stdout() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("echo hello-tests", td.path(), 5_000).await.unwrap();
        assert!(r.stdout.contains("hello-tests"));
    }

    #[tokio::test]
    async fn runs_inside_the_project_root() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("marker.txt"), "i-am-here").unwrap();
        // If cwd is the project root, `cat marker.txt` finds the file.
        let r = run_in_dir("cat marker.txt", td.path(), 5_000).await.unwrap();
        assert!(r.passed, "stderr={}", r.stderr);
        assert!(r.stdout.contains("i-am-here"));
    }

    #[tokio::test]
    async fn output_tail_is_kept_when_truncated() {
        let td = TempDir::new().unwrap();
        // Print many numbered lines; the *last* line is what matters for a test
        // summary, so the kept tail must contain it and not the first line.
        let cmd = "for i in $(seq 1 5000); do echo \"line-$i marker\"; done";
        let r = run_in_dir(cmd, td.path(), 10_000).await.unwrap();
        assert!(r.truncated, "expected the long output to be clipped");
        assert!(r.stdout.contains("line-5000 marker"), "tail must keep the last line");
        assert!(!r.stdout.contains("line-1 marker"), "head should be dropped");
        assert!(r.stdout.len() <= MAX_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn empty_command_rejected() {
        let td = TempDir::new().unwrap();
        let err = run_in_dir("   ", td.path(), 5_000).await.unwrap_err();
        assert!(err.contains("empty"));
    }

    #[tokio::test]
    async fn timeout_kills_and_reports() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("sleep 5", td.path(), 200).await.unwrap();
        assert!(r.timed_out);
        assert!(!r.passed);
        assert_eq!(r.exit_code, None);
    }

    #[tokio::test]
    async fn run_command_errors_when_unset() {
        let td = TempDir::new().unwrap();
        let err = run_test_command(td.path().display().to_string())
            .await
            .unwrap_err();
        assert!(err.contains("no test command configured"));
    }

    #[tokio::test]
    async fn run_command_end_to_end_uses_configured_command() {
        let td = TempDir::new().unwrap();
        write_test_command(td.path(), "echo configured-run && exit 0").unwrap();
        let r = run_test_command(td.path().display().to_string())
            .await
            .unwrap();
        assert!(r.passed);
        assert!(r.stdout.contains("configured-run"));
        assert_eq!(r.command, "echo configured-run && exit 0");
    }

    #[tokio::test]
    async fn run_command_rejects_non_directory_root() {
        let err = run_test_command("/no/such/dir/xyz".into())
            .await
            .unwrap_err();
        assert!(err.contains("not a directory"));
    }
}
