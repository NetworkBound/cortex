//! Aider-style **lint command** — `/lint` (aider's `--lint-cmd` / `--auto-lint`).
//!
//! Aider can run a linter over the project and feed any violations back to the
//! model. It either uses a per-language built-in (`flake8`, `cargo clippy`, …)
//! or a user-configured `--lint-cmd`. Cortex already has the *test* half of that
//! loop ([`super::auto_test`]); this is the lint half.
//!
//! This module owns:
//!   * **auto-detection** of a sensible linter for the project from the marker
//!     files at its root (a `package.json` `lint` script, an ESLint config,
//!     Ruff config, `Cargo.toml`, `go.mod`), via [`detect_lint_command`];
//!   * a **per-project override** persisted at
//!     `<project_root>/.cortex/lint-command.toml`:
//!     ```toml
//!     command = "cargo clippy -- -D warnings"
//!     ```
//!     (set with `/lintcmd`, mirroring `/testcmd`);
//!   * running the resolved command inside the project root, capturing a bounded
//!     snapshot of its output under a wall-clock timeout, and reporting the exit
//!     code.
//!
//! ## Why the output **head**, not the tail
//! [`super::auto_test`] keeps the output *tail* because a test run's failures and
//! summary live at the end. Lint output is the opposite: linters (clippy, ESLint,
//! Ruff) stream violations top-to-bottom and put a count line at the very end, so
//! the *first* violations are the ones you fix first. When clipped we keep the
//! **head** so you see the leading violations rather than only the trailing ones.
//!
//! ## Honest about exit codes
//! Unlike tests, a clean exit code does not always mean "no findings" — `cargo
//! clippy` exits 0 even with warnings (only `-D warnings` makes it fail). So the
//! frontend always shows the captured output even on exit 0, and a user who wants
//! a hard gate can configure a strict override (`cargo clippy -- -D warnings`).
//!
//! The detection, config load/write, and run-in-dir core are pure aside from the
//! filesystem / subprocess and are unit-tested on real temp dirs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Hard cap on combined stdout+stderr bytes returned to the frontend. Matches
/// [`super::auto_test::MAX_OUTPUT_BYTES`] — lint output can be long but bounded
/// keeps the chat render smooth. We keep the **head** (see module docs).
pub const MAX_OUTPUT_BYTES: usize = 24 * 1024;

/// Wall-clock timeout for a lint run. Linters (clippy especially, which compiles)
/// are slow, so this is generous (2 min); on elapse we kill the child.
pub const TIMEOUT_MS: u64 = 120_000;

/// On-disk schema for `.cortex/lint-command.toml`.
#[derive(Debug, Deserialize, Serialize, Default)]
struct LintCommandFile {
    command: Option<String>,
}

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("lint-command.toml")
}

/// Load the configured lint-command **override** for a project, or `None` if not
/// set. A missing file, malformed TOML, or a blank command all read as `None` so
/// a project that never opts in falls through to auto-detection.
pub fn load_lint_command(project_root: &Path) -> Option<String> {
    let path = config_path(project_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let parsed: LintCommandFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("lint: bad toml at {}: {e}", path.display());
            return None;
        }
    };
    parsed
        .command
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}

/// Persist (or clear) the lint-command override for a project, creating
/// `.cortex/` if needed. A blank command **clears** the override by removing the
/// file, so the "use auto-detection" state has a single representation (no file).
pub fn write_lint_command(project_root: &Path, command: &str) -> anyhow::Result<()> {
    let path = config_path(project_root);
    let trimmed = command.trim();
    if trimmed.is_empty() {
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

/// Render a string as a TOML basic-string literal so a command containing
/// quotes/backslashes round-trips.
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

/// True when `package.json` at `root` declares a `scripts.lint` string. We
/// respect the project's own lint script when present — it's the most accurate
/// linter for that project (the right config, plugins, and ignores already set).
fn has_npm_lint_script(root: &Path) -> bool {
    let raw = match std::fs::read_to_string(root.join("package.json")) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    json.get("scripts")
        .and_then(|s| s.get("lint"))
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// True when an ESLint config file is present at `root` (flat or legacy).
fn has_eslint_config(root: &Path) -> bool {
    const NAMES: &[&str] = &[
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
        "eslint.config.ts",
        ".eslintrc.js",
        ".eslintrc.cjs",
        ".eslintrc.json",
        ".eslintrc.yml",
        ".eslintrc.yaml",
        ".eslintrc",
    ];
    NAMES.iter().any(|n| root.join(n).is_file())
}

/// True when Ruff config is present — a dedicated `ruff.toml`/`.ruff.toml`, or a
/// `[tool.ruff]` table in `pyproject.toml`.
fn has_ruff_config(root: &Path) -> bool {
    if root.join("ruff.toml").is_file() || root.join(".ruff.toml").is_file() {
        return true;
    }
    std::fs::read_to_string(root.join("pyproject.toml"))
        .map(|s| s.contains("[tool.ruff"))
        .unwrap_or(false)
}

/// Auto-detect a sensible lint command from the marker files at the project root,
/// or `None` when nothing recognizable is present. Priority is "what the project
/// itself defines" first (its own `npm run lint`), then well-known per-language
/// linters. The first match wins, so a polyglot repo's root language is picked.
pub fn detect_lint_command(root: &Path) -> Option<String> {
    if has_npm_lint_script(root) {
        return Some("npm run lint".to_string());
    }
    if has_eslint_config(root) {
        // `--no-install` so we never silently fetch eslint from the network.
        return Some("npx --no-install eslint .".to_string());
    }
    if has_ruff_config(root) {
        return Some("ruff check .".to_string());
    }
    if root.join("Cargo.toml").is_file() {
        return Some("cargo clippy --quiet --all-targets".to_string());
    }
    if root.join("go.mod").is_file() {
        return Some("go vet ./...".to_string());
    }
    None
}

/// Resolve the lint command to run: the persisted override wins, else
/// auto-detection. `None` when there's no override and nothing detectable.
pub fn resolve_lint_command(root: &Path) -> Option<String> {
    load_lint_command(root).or_else(|| detect_lint_command(root))
}

/// Outcome of running the project's lint command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LintRunOutcome {
    /// The command that was run.
    pub command: String,
    /// True when this came from the persisted override (vs auto-detection).
    pub from_override: bool,
    /// Process exit code; `None` when it timed out or was killed.
    pub exit_code: Option<i32>,
    /// True when the command exited 0. NOTE: for some linters (e.g. plain
    /// `cargo clippy`) this can be 0 even with warnings — the frontend always
    /// shows the output regardless.
    pub clean: bool,
    /// Head of stdout (first `MAX_OUTPUT_BYTES`).
    pub stdout: String,
    /// Head of stderr (first `MAX_OUTPUT_BYTES`).
    pub stderr: String,
    pub duration_ms: u64,
    /// True when output was clipped (we kept the head).
    pub truncated: bool,
    /// True when the wall-clock budget elapsed and we killed the child.
    pub timed_out: bool,
}

/// Keep the **head** of `s` within `cap` bytes (on a UTF-8 char boundary).
/// Returns true when anything was dropped. Head, not tail, because a lint run's
/// leading violations are the ones you fix first (see module docs).
fn clip_head(s: &mut String, cap: usize) -> bool {
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

/// Run `command` inside `cwd` with a timeout, returning a bounded snapshot.
/// Factored out so it's testable with trivial shell commands on a temp dir.
async fn run_in_dir(
    command: &str,
    from_override: bool,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<LintRunOutcome, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty lint command".to_string());
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
            let t1 = clip_head(&mut stdout, MAX_OUTPUT_BYTES);
            let t2 = clip_head(&mut stderr, MAX_OUTPUT_BYTES);
            let exit_code = out.status.code();
            Ok(LintRunOutcome {
                command: cmd.to_string(),
                from_override,
                exit_code,
                clean: exit_code == Some(0),
                stdout,
                stderr,
                duration_ms,
                truncated: t1 || t2,
                timed_out: false,
            })
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => Ok(LintRunOutcome {
            command: cmd.to_string(),
            from_override,
            exit_code: None,
            clean: false,
            stdout: String::new(),
            stderr: format!("[lint] timed out after {timeout_ms}ms"),
            duration_ms,
            truncated: false,
            timed_out: true,
        }),
    }
}

// ---- Tauri commands -----------------------------------------------------

/// Return the configured lint-command **override** for a project, or `null` if
/// unset (the project then uses auto-detection).
#[tauri::command]
pub async fn get_lint_command(project_root: String) -> Result<Option<String>, String> {
    let root = PathBuf::from(&project_root);
    Ok(load_lint_command(&root))
}

/// Return the lint command Cortex *would* run for a project (override or
/// auto-detected), or `null` when nothing is detectable. Lets the UI show what
/// `/lint` will do before running it.
#[tauri::command]
pub async fn detect_lint(project_root: String) -> Result<Option<String>, String> {
    let root = PathBuf::from(&project_root);
    Ok(resolve_lint_command(&root))
}

/// Set (or clear, with a blank string) the project's lint-command override.
#[tauri::command]
pub async fn set_lint_command(project_root: String, command: String) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    write_lint_command(&root, &command).map_err(|e| format!("write failed: {e}"))
}

/// Run the project's lint command (override or auto-detected) and return the
/// outcome. Errors when no command is configured *and* none could be detected
/// (the frontend uses that to prompt `/lintcmd`).
#[tauri::command]
pub async fn run_lint(project_root: String) -> Result<LintRunOutcome, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let from_override = load_lint_command(&root).is_some();
    let cmd = resolve_lint_command(&root).ok_or_else(|| {
        "no linter detected — set one with /lintcmd <command> (e.g. /lintcmd cargo clippy)"
            .to_string()
    })?;
    run_in_dir(&cmd, from_override, &root, TIMEOUT_MS).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, name: &str, body: &str) {
        std::fs::write(root.join(name), body).unwrap();
    }

    #[test]
    fn write_then_load_round_trips() {
        let td = TempDir::new().unwrap();
        write_lint_command(td.path(), "cargo clippy").unwrap();
        assert_eq!(load_lint_command(td.path()).as_deref(), Some("cargo clippy"));
    }

    #[test]
    fn load_missing_is_none() {
        let td = TempDir::new().unwrap();
        assert_eq!(load_lint_command(td.path()), None);
    }

    #[test]
    fn blank_command_clears_the_override() {
        let td = TempDir::new().unwrap();
        write_lint_command(td.path(), "ruff check .").unwrap();
        assert!(load_lint_command(td.path()).is_some());
        write_lint_command(td.path(), "   ").unwrap();
        assert_eq!(load_lint_command(td.path()), None);
        write_lint_command(td.path(), "").unwrap();
        assert_eq!(load_lint_command(td.path()), None);
    }

    #[test]
    fn command_with_quotes_round_trips() {
        let td = TempDir::new().unwrap();
        let cmd = r#"eslint "src/**/*.ts" --max-warnings 0"#;
        write_lint_command(td.path(), cmd).unwrap();
        assert_eq!(load_lint_command(td.path()).as_deref(), Some(cmd));
    }

    #[test]
    fn detect_none_in_empty_repo() {
        let td = TempDir::new().unwrap();
        assert_eq!(detect_lint_command(td.path()), None);
    }

    #[test]
    fn detect_npm_lint_script_wins() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "package.json",
            r#"{"name":"x","scripts":{"lint":"eslint src","build":"vite"}}"#,
        );
        // Even with a Cargo.toml also present, the project's own lint script wins.
        write(td.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        assert_eq!(detect_lint_command(td.path()).as_deref(), Some("npm run lint"));
    }

    #[test]
    fn package_json_without_lint_script_does_not_match() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "package.json",
            r#"{"name":"x","scripts":{"build":"vite"}}"#,
        );
        // No lint script and no eslint config → falls through to None here.
        assert_eq!(detect_lint_command(td.path()), None);
    }

    #[test]
    fn malformed_package_json_falls_through() {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{ not json");
        write(td.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        // A broken package.json must not crash detection; falls through to cargo.
        assert_eq!(
            detect_lint_command(td.path()).as_deref(),
            Some("cargo clippy --quiet --all-targets")
        );
    }

    #[test]
    fn detect_eslint_config() {
        let td = TempDir::new().unwrap();
        write(td.path(), "eslint.config.js", "export default [];");
        assert_eq!(
            detect_lint_command(td.path()).as_deref(),
            Some("npx --no-install eslint .")
        );
    }

    #[test]
    fn detect_ruff_via_dedicated_file_and_pyproject() {
        let td = TempDir::new().unwrap();
        write(td.path(), "ruff.toml", "line-length = 100\n");
        assert_eq!(detect_lint_command(td.path()).as_deref(), Some("ruff check ."));

        let td2 = TempDir::new().unwrap();
        write(
            td2.path(),
            "pyproject.toml",
            "[tool.ruff]\nline-length = 100\n",
        );
        assert_eq!(detect_lint_command(td2.path()).as_deref(), Some("ruff check ."));

        // pyproject without a [tool.ruff] table does not match Ruff.
        let td3 = TempDir::new().unwrap();
        write(td3.path(), "pyproject.toml", "[tool.black]\n");
        assert_eq!(detect_lint_command(td3.path()), None);
    }

    #[test]
    fn detect_cargo_then_go() {
        let td = TempDir::new().unwrap();
        write(td.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        assert_eq!(
            detect_lint_command(td.path()).as_deref(),
            Some("cargo clippy --quiet --all-targets")
        );

        let td2 = TempDir::new().unwrap();
        write(td2.path(), "go.mod", "module x\n");
        assert_eq!(detect_lint_command(td2.path()).as_deref(), Some("go vet ./..."));
    }

    #[test]
    fn override_wins_over_detection() {
        let td = TempDir::new().unwrap();
        write(td.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write_lint_command(td.path(), "cargo clippy -- -D warnings").unwrap();
        assert_eq!(
            resolve_lint_command(td.path()).as_deref(),
            Some("cargo clippy -- -D warnings")
        );
    }

    #[tokio::test]
    async fn clean_command_reports_clean() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("exit 0", false, td.path(), 5_000).await.unwrap();
        assert!(r.clean);
        assert_eq!(r.exit_code, Some(0));
        assert!(!r.from_override);
    }

    #[tokio::test]
    async fn dirty_command_reports_not_clean_with_code() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("exit 1", true, td.path(), 5_000).await.unwrap();
        assert!(!r.clean);
        assert_eq!(r.exit_code, Some(1));
        assert!(r.from_override);
    }

    #[tokio::test]
    async fn runs_inside_the_project_root() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("marker.txt"), "i-am-here").unwrap();
        let r = run_in_dir("cat marker.txt", false, td.path(), 5_000)
            .await
            .unwrap();
        assert!(r.clean, "stderr={}", r.stderr);
        assert!(r.stdout.contains("i-am-here"));
    }

    #[tokio::test]
    async fn output_head_is_kept_when_truncated() {
        let td = TempDir::new().unwrap();
        // The *first* violations matter for lint, so the kept head must contain
        // the first line and drop the last.
        let cmd = "for i in $(seq 1 5000); do echo \"line-$i marker\"; done";
        let r = run_in_dir(cmd, false, td.path(), 10_000).await.unwrap();
        assert!(r.truncated, "expected the long output to be clipped");
        assert!(r.stdout.contains("line-1 marker"), "head must keep the first line");
        assert!(!r.stdout.contains("line-5000 marker"), "tail should be dropped");
        assert!(r.stdout.len() <= MAX_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn empty_command_rejected() {
        let td = TempDir::new().unwrap();
        let err = run_in_dir("   ", false, td.path(), 5_000).await.unwrap_err();
        assert!(err.contains("empty"));
    }

    #[tokio::test]
    async fn timeout_kills_and_reports() {
        let td = TempDir::new().unwrap();
        let r = run_in_dir("sleep 5", false, td.path(), 200).await.unwrap();
        assert!(r.timed_out);
        assert!(!r.clean);
        assert_eq!(r.exit_code, None);
    }

    #[tokio::test]
    async fn run_lint_errors_when_nothing_detectable() {
        let td = TempDir::new().unwrap();
        let err = run_lint(td.path().display().to_string())
            .await
            .unwrap_err();
        assert!(err.contains("no linter detected"));
    }

    #[tokio::test]
    async fn run_lint_end_to_end_uses_override() {
        let td = TempDir::new().unwrap();
        write_lint_command(td.path(), "echo lint-override-ran && exit 0").unwrap();
        let r = run_lint(td.path().display().to_string()).await.unwrap();
        assert!(r.clean);
        assert!(r.from_override);
        assert!(r.stdout.contains("lint-override-ran"));
    }

    #[tokio::test]
    async fn run_lint_rejects_non_directory_root() {
        let err = run_lint("/no/such/dir/xyz".into()).await.unwrap_err();
        assert!(err.contains("not a directory"));
    }
}
