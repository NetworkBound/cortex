//! Inline test runner.
//!
//! Auto-detects the project's test framework (Cargo / Vitest / Jest / Mocha /
//! Pytest), runs it via [`std::process::Command`], caps the captured output
//! at 64 KiB, and parses summary counts + failures into a structured
//! [`TestResult`] the frontend can render as a panel.
//!
//! The detection is best-effort and conservative — when nothing matches we
//! return `error: "no test framework detected"` rather than guessing.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

/// Cap on captured stdout / stderr. 64 KiB is plenty for summary parsing while
/// keeping the JSON payload manageable across the Tauri bridge.
const OUTPUT_CAP_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    /// Test name (e.g. `tests::it_works` for Cargo, `MyComponent > renders`
    /// for Vitest). Best-effort.
    pub name: String,
    /// File path + optional `:line` when the framework prints it.
    pub location: Option<String>,
    /// One-line failure message (e.g. assertion text). Empty when the
    /// framework's output is too terse to extract one.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// Detected framework label: `"cargo"`, `"vitest"`, `"jest"`, `"mocha"`,
    /// `"pytest"`.
    pub framework: String,
    /// Exact command we ran (joined for display only — the backend always
    /// spawns via argv).
    pub command: String,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_code: i32,
    pub failures: Vec<TestFailure>,
}

#[tauri::command]
pub async fn run_tests(
    project_root: String,
    framework: Option<String>,
) -> Result<TestResult, String> {
    let root = Path::new(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let pick = match framework.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(explicit) => pick_explicit(explicit, root)?,
        None => detect_framework(root)
            .ok_or_else(|| "no test framework detected".to_string())?,
    };

    let started = Instant::now();
    let display_cmd = format!("{} {}", pick.argv[0], pick.argv[1..].join(" "));
    let output = crate::sys::no_window(&pick.argv[0])
        .args(&pick.argv[1..])
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to spawn {}: {e}", pick.argv[0]))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    let (passed, failed, skipped) = parse_counts(&pick.framework, &combined);
    let failures = parse_failures(&pick.framework, &combined);

    Ok(TestResult {
        framework: pick.framework,
        command: display_cmd,
        passed,
        failed,
        skipped,
        duration_ms,
        stdout_tail: tail(&stdout, OUTPUT_CAP_BYTES),
        stderr_tail: tail(&stderr, OUTPUT_CAP_BYTES),
        exit_code: output.status.code().unwrap_or(-1),
        failures,
    })
}

struct Pick {
    framework: String,
    argv: Vec<String>,
}

fn pick_explicit(name: &str, root: &Path) -> Result<Pick, String> {
    match name {
        "cargo" => Ok(Pick {
            framework: "cargo".into(),
            argv: vec!["cargo".into(), "test".into(), "--no-fail-fast".into()],
        }),
        "vitest" => Ok(Pick {
            framework: "vitest".into(),
            argv: npm_run(root, "vitest", &["run"])?,
        }),
        "jest" => Ok(Pick {
            framework: "jest".into(),
            argv: npm_run(root, "jest", &[])?,
        }),
        "mocha" => Ok(Pick {
            framework: "mocha".into(),
            argv: npm_run(root, "mocha", &[])?,
        }),
        "pytest" => Ok(Pick {
            framework: "pytest".into(),
            argv: vec!["pytest".into()],
        }),
        other => Err(format!("unsupported framework: {other}")),
    }
}

fn detect_framework(root: &Path) -> Option<Pick> {
    if root.join("Cargo.toml").is_file() {
        return Some(Pick {
            framework: "cargo".into(),
            argv: vec!["cargo".into(), "test".into(), "--no-fail-fast".into()],
        });
    }
    let pkg = root.join("package.json");
    if pkg.is_file() {
        if let Ok(raw) = std::fs::read_to_string(&pkg) {
            if let Some(fw) = detect_node_framework(&raw) {
                // `npm_run` returns Err when the framework isn't installed
                // locally (it refuses to auto-download from the registry). In
                // auto-detection we treat that as "nothing runnable here" and
                // fall through, keeping detection best-effort and side-effect
                // free.
                let argv = match fw.as_str() {
                    "vitest" => npm_run(root, "vitest", &["run"]).ok()?,
                    "jest" => npm_run(root, "jest", &[]).ok()?,
                    "mocha" => npm_run(root, "mocha", &[]).ok()?,
                    _ => return None,
                };
                return Some(Pick { framework: fw, argv });
            }
        }
    }
    // Python detection — pytest.ini or [tool.pytest] section in pyproject.toml.
    if root.join("pytest.ini").is_file() {
        return Some(Pick {
            framework: "pytest".into(),
            argv: vec!["pytest".into()],
        });
    }
    let pyproject = root.join("pyproject.toml");
    if pyproject.is_file() {
        if let Ok(raw) = std::fs::read_to_string(&pyproject) {
            if raw.contains("[tool.pytest") {
                return Some(Pick {
                    framework: "pytest".into(),
                    argv: vec!["pytest".into()],
                });
            }
        }
    }
    None
}

/// Inspect a `package.json` blob for vitest / jest / mocha references. We
/// check both dep maps and the `scripts.test` string so projects that only
/// pin the framework via a script alias still get detected.
fn detect_node_framework(raw: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let mut haystack = String::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = val.get(key).and_then(|v| v.as_object()) {
            for k in obj.keys() {
                haystack.push_str(k);
                haystack.push(' ');
            }
        }
    }
    if let Some(scripts) = val.get("scripts").and_then(|v| v.as_object()) {
        if let Some(test) = scripts.get("test").and_then(|v| v.as_str()) {
            haystack.push_str(test);
        }
    }
    // Order matters — prefer vitest over jest over mocha when multiple are
    // present (vitest is the modern default).
    for fw in ["vitest", "jest", "mocha"] {
        if haystack.contains(fw) {
            return Some(fw.to_string());
        }
    }
    None
}

/// Build an `npm run test` / `npm exec` invocation. We prefer the project's
/// own `scripts.test` when it mentions the framework so user-set flags
/// (`--reporter=verbose`, `--coverage`, …) are honored.
///
/// Security: the `project_root` is untrusted. We must never silently download
/// and execute an arbitrary package from the npm registry. We therefore only
/// fall back to `npx` when the framework binary is already present in the
/// project's local `node_modules/.bin`, and we pass `--no-install` so npx will
/// refuse (rather than fetch + run remote code) if it somehow isn't. When the
/// framework isn't installed locally, the caller surfaces an actionable error
/// instead of running anything.
fn npm_run(root: &Path, framework: &str, extra: &[&str]) -> Result<Vec<String>, String> {
    if let Ok(raw) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(test) = v
                .get("scripts")
                .and_then(|s| s.get("test"))
                .and_then(|t| t.as_str())
            {
                if test.contains(framework) {
                    return Ok(vec!["npm".into(), "test".into(), "--silent".into()]);
                }
            }
        }
    }
    if !framework_installed_locally(root, framework) {
        return Err(format!(
            "{framework} is not installed in this project; refusing to auto-download \
             and run it from the npm registry. Run `npm install` (or add a `test` \
             script) first."
        ));
    }
    // `--no-install` guarantees npx executes the locally installed binary and
    // never reaches out to the registry to fetch+run untrusted code.
    let mut argv = vec![
        "npx".into(),
        "--no-install".into(),
        framework.into(),
    ];
    for e in extra {
        argv.push((*e).into());
    }
    Ok(argv)
}

/// True when `framework`'s executable is present in the project's local
/// `node_modules/.bin`. Checks the bare name plus common Windows shims so we
/// don't network-fetch a tool the user hasn't actually installed.
fn framework_installed_locally(root: &Path, framework: &str) -> bool {
    let bin = root.join("node_modules").join(".bin");
    bin.join(framework).is_file()
        || bin.join(format!("{framework}.cmd")).is_file()
        || bin.join(format!("{framework}.ps1")).is_file()
        || root.join("node_modules").join(framework).is_dir()
}

static CARGO_COUNTS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"test result:.*?(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored")
        .expect("cargo regex")
});
static VITEST_COUNTS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"Tests\s+(?:(\d+)\s+failed\s*\|\s*)?(\d+)\s+passed(?:\s*\|\s*(\d+)\s+skipped)?")
        .expect("vitest regex")
});
static JEST_COUNTS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"Tests:\s+(?:(\d+)\s+failed,\s*)?(?:(\d+)\s+skipped,\s*)?(\d+)\s+passed")
        .expect("jest regex")
});
static MOCHA_PASS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+passing").expect("mocha pass regex"));
static MOCHA_FAIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+failing").expect("mocha fail regex"));
static MOCHA_SKIP: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+pending").expect("mocha skip regex"));
// pytest's summary line lists counts in no fixed order and may omit any of
// them (e.g. "2 failed, 3 passed, 1 skipped" or just "5 passed"). Match each
// keyword independently with a required `\d+`, so we never match an empty
// string or pick up a number that belongs to a different keyword.
static PYTEST_PASS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+passed").expect("pytest pass regex"));
static PYTEST_FAIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+failed").expect("pytest fail regex"));
static PYTEST_SKIP: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)\s+skipped").expect("pytest skip regex"));

/// Best-effort `(passed, failed, skipped)` extraction. When no pattern matches
/// we return zeros — the caller still surfaces the raw output tail so users
/// can eyeball it.
fn parse_counts(framework: &str, output: &str) -> (u32, u32, u32) {
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    let last = |re: &Regex, idx: usize| -> u32 {
        re.captures_iter(output)
            .last()
            .and_then(|c| c.get(idx))
            .map(|m| n(m.as_str()))
            .unwrap_or(0)
    };
    match framework {
        "cargo" => {
            // Sum across multiple `test result:` lines (cargo prints one per
            // crate). Iterating gives us the totals from a workspace run.
            let (mut p, mut f, mut s) = (0u32, 0u32, 0u32);
            for cap in CARGO_COUNTS.captures_iter(output) {
                p += n(&cap[1]);
                f += n(&cap[2]);
                s += n(&cap[3]);
            }
            (p, f, s)
        }
        "vitest" => {
            let f = last(&VITEST_COUNTS, 1);
            let p = last(&VITEST_COUNTS, 2);
            let s = last(&VITEST_COUNTS, 3);
            (p, f, s)
        }
        "jest" => {
            let f = last(&JEST_COUNTS, 1);
            let s = last(&JEST_COUNTS, 2);
            let p = last(&JEST_COUNTS, 3);
            (p, f, s)
        }
        "mocha" => (
            last(&MOCHA_PASS, 1),
            last(&MOCHA_FAIL, 1),
            last(&MOCHA_SKIP, 1),
        ),
        "pytest" => (
            last(&PYTEST_PASS, 1),
            last(&PYTEST_FAIL, 1),
            last(&PYTEST_SKIP, 1),
        ),
        _ => (0, 0, 0),
    }
}

static CARGO_FAIL_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^---- ([^\s]+) stdout ----").expect("cargo fail regex"));
static VITEST_FAIL_LINE: Lazy<Regex> = Lazy::new(|| {
    // Matches lines like " FAIL  src/foo.test.ts > suite > case" with optional
    // leading whitespace.
    Regex::new(r"(?m)^\s*(?:FAIL|×|✗)\s+(\S+)(?:\s+>\s+(.+))?$").expect("vitest fail regex")
});
static PYTEST_FAIL_LINE: Lazy<Regex> = Lazy::new(|| {
    // pytest verbose: "FAILED tests/test_foo.py::test_bar - AssertionError: …"
    Regex::new(r"(?m)^FAILED\s+(\S+?)(?:::(\S+))?(?:\s+-\s+(.*))?$").expect("pytest fail regex")
});

/// Pull out per-test failure entries. We cap at 50 to keep the modal scroll
/// reasonable even when a test suite explodes.
fn parse_failures(framework: &str, output: &str) -> Vec<TestFailure> {
    let mut out = Vec::new();
    match framework {
        "cargo" => {
            for cap in CARGO_FAIL_LINE.captures_iter(output) {
                if out.len() >= 50 {
                    break;
                }
                let name = cap[1].to_string();
                // Look ahead a few lines for a `thread '…' panicked at …:line`
                // location stamp + the panic message right after.
                let after_idx = cap.get(0).map(|m| m.end()).unwrap_or(0);
                let window: String = output[after_idx..]
                    .lines()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n");
                let location = extract_cargo_location(&window);
                let message = extract_cargo_message(&window);
                out.push(TestFailure { name, location, message });
            }
        }
        "vitest" | "jest" => {
            for cap in VITEST_FAIL_LINE.captures_iter(output) {
                if out.len() >= 50 {
                    break;
                }
                let file = cap[1].to_string();
                let suite_case = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                let name = if suite_case.is_empty() {
                    file.clone()
                } else {
                    suite_case
                };
                out.push(TestFailure {
                    name,
                    location: Some(file),
                    message: String::new(),
                });
            }
        }
        "pytest" => {
            for cap in PYTEST_FAIL_LINE.captures_iter(output) {
                if out.len() >= 50 {
                    break;
                }
                let file = cap[1].to_string();
                let case = cap.get(2).map(|m| m.as_str().to_string());
                let msg = cap.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
                out.push(TestFailure {
                    name: case.unwrap_or_else(|| file.clone()),
                    location: Some(file),
                    message: msg,
                });
            }
        }
        _ => {}
    }
    out
}

static CARGO_LOCATION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"at\s+([^\s:]+):(\d+)").expect("cargo location regex"));

fn extract_cargo_location(window: &str) -> Option<String> {
    CARGO_LOCATION
        .captures(window)
        .map(|c| format!("{}:{}", &c[1], &c[2]))
}

fn extract_cargo_message(window: &str) -> String {
    for line in window.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("thread '") && trimmed.contains("panicked at") {
            // The next line is usually the actual assertion.
            continue;
        }
        if trimmed.starts_with("assertion") || trimmed.starts_with("panicked at") {
            return trimmed.to_string();
        }
    }
    window.lines().next().unwrap_or("").trim().to_string()
}

/// Keep the trailing `limit` bytes of `s` so the most recent output (the
/// failure summary) stays visible. Splits on a UTF-8 boundary.
fn tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut cut = s.len() - limit;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    let mut out = String::from("…[truncated]\n");
    out.push_str(&s[cut..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_counts_summed_across_lines() {
        let out = "\
test result: ok. 5 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
";
        assert_eq!(parse_counts("cargo", out), (8, 2, 1));
    }

    #[test]
    fn vitest_counts_basic() {
        let out = " Tests  2 failed | 5 passed | 1 skipped (8)";
        assert_eq!(parse_counts("vitest", out), (5, 2, 1));
    }

    #[test]
    fn jest_counts_basic() {
        let out = "Tests: 1 failed, 2 skipped, 7 passed, 10 total";
        assert_eq!(parse_counts("jest", out), (7, 1, 2));
    }

    #[test]
    fn mocha_counts_basic() {
        let out = "  3 passing (12ms)\n  1 failing\n  2 pending\n";
        assert_eq!(parse_counts("mocha", out), (3, 1, 2));
    }

    #[test]
    fn detect_node_picks_vitest_over_jest() {
        let raw = r#"{"devDependencies":{"vitest":"1","jest":"29"}}"#;
        assert_eq!(detect_node_framework(raw).as_deref(), Some("vitest"));
    }

    #[test]
    fn detect_node_via_scripts_test() {
        let raw = r#"{"scripts":{"test":"jest --coverage"}}"#;
        assert_eq!(detect_node_framework(raw).as_deref(), Some("jest"));
    }

    #[test]
    fn tail_keeps_trailing_bytes() {
        let big = "x".repeat(100_000);
        let t = tail(&big, 64 * 1024);
        assert!(t.starts_with("…[truncated]"));
        assert!(t.len() <= 64 * 1024 + 20);
    }

    #[test]
    fn cargo_failure_extraction() {
        let out = "\
---- tests::it_works stdout ----
thread 'tests::it_works' panicked at src/lib.rs:42:9:
assertion `left == right` failed
  left: 1
  right: 2
";
        let f = parse_failures("cargo", out);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "tests::it_works");
        assert_eq!(f[0].location.as_deref(), Some("src/lib.rs:42"));
        assert!(f[0].message.contains("assertion"));
    }

    #[test]
    fn pytest_failure_extraction() {
        let out = "FAILED tests/test_foo.py::test_bar - AssertionError: boom\n";
        let f = parse_failures("pytest", out);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "test_bar");
        assert_eq!(f[0].location.as_deref(), Some("tests/test_foo.py"));
        assert!(f[0].message.contains("boom"));
    }
}
