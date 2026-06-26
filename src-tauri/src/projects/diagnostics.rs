//! Project diagnostics collector (Continue-style `@problems` provider).
//!
//! Runs the project's compilers in check-only mode and parses their output
//! into a uniform [`Diagnostic`] shape so the @-vocab picker can surface a
//! flat list of recent issues. This is a *snapshot* — we shell out, parse,
//! and return; there is no live LSP / file-watch loop. To avoid hammering
//! the compiler when the user spams `@problems`, results are cached for
//! `CACHE_TTL` per project root.
//!
//! Currently supported:
//!   - Rust:       `cargo check --message-format=json` (when `Cargo.toml` exists)
//!   - TypeScript: `npx tsc --noEmit`                  (when `tsconfig.json` exists)
//!
//! Each diagnostic is capped at 100 per call; older entries are dropped so
//! the picker stays scrollable.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// One compile error / warning, normalised across toolchains.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Tool that produced this diagnostic (`cargo` | `tsc`).
    pub source: String,
    /// `error` | `warning` | `note`.
    pub severity: String,
    /// Absolute or project-relative path to the offending file.
    pub path: String,
    /// 1-indexed line number; 0 if unknown.
    pub line: u32,
    /// Human-readable message.
    pub message: String,
}

/// Hard upper bound on the number of diagnostics returned per call. Keeps
/// the picker scrollable when a project has hundreds of warnings.
const MAX_DIAGNOSTICS: usize = 100;

/// How long results are cached per project root before we re-run compilers.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Hard wall-clock budget for a single compiler invocation. `@problems` is a
/// best-effort snapshot, not a build gate: a project whose `cargo check` /
/// `tsc` hangs (huge crate graph, runaway build script, intentionally slow
/// toolchain) must not be able to block the caller indefinitely. If the child
/// outlives this budget we kill it and return whatever we have (nothing).
const COMPILE_TIMEOUT: Duration = Duration::from_secs(20);

static CACHE: Lazy<Mutex<HashMap<PathBuf, (Instant, Vec<Diagnostic>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Collect diagnostics for `project_root`, using the cache when fresh.
/// Returns an empty vec if neither `Cargo.toml` nor `tsconfig.json` exist —
/// no project, nothing to report.
pub fn collect(project_root: &Path) -> Vec<Diagnostic> {
    if let Some(cached) = read_cache(project_root) {
        return cached;
    }

    let mut diags: Vec<Diagnostic> = Vec::new();
    if project_root.join("Cargo.toml").exists() {
        diags.extend(run_cargo_check(project_root));
    }
    if project_root.join("tsconfig.json").exists() {
        diags.extend(run_tsc(project_root));
    }
    if diags.len() > MAX_DIAGNOSTICS {
        diags.truncate(MAX_DIAGNOSTICS);
    }

    write_cache(project_root, diags.clone());
    diags
}

fn read_cache(root: &Path) -> Option<Vec<Diagnostic>> {
    let cache = CACHE.lock();
    let (when, diags) = cache.get(root)?;
    if when.elapsed() < CACHE_TTL {
        Some(diags.clone())
    } else {
        None
    }
}

fn write_cache(root: &Path, diags: Vec<Diagnostic>) {
    let mut cache = CACHE.lock();
    cache.insert(root.to_path_buf(), (Instant::now(), diags));
}

/// Spawn `cmd args` in `root`, capturing stdout, but bound the wall-clock time
/// to [`COMPILE_TIMEOUT`]. `std::process::Command` has no native timeout, so we
/// spawn, drain stdout on a reader thread (the child can produce more than the
/// pipe buffer holds), and poll `try_wait`. On overrun we kill *and reap* the
/// child so it can't wedge the caller or leak a zombie/file descriptors.
/// Returns the captured stdout, or `None` if the child was killed or failed.
fn run_capture(cmd: &str, args: &[&str], root: &Path) -> Option<String> {
    let mut child = crate::sys::no_window(cmd)
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let stdout_handle = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let stdout = stdout_handle
                    .and_then(|h| h.join().ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                return Some(stdout);
            }
            Ok(None) => {
                if start.elapsed() > COMPILE_TIMEOUT {
                    let _ = child.kill();
                    // Reap so we don't leak a zombie; then join the reader so we
                    // don't leak the thread either.
                    let _ = child.wait();
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// Run `cargo check --message-format=json` and parse each NDJSON line into
/// at most one [`Diagnostic`]. Errors at the process layer (cargo missing,
/// non-zero exit with no JSON) are silently swallowed — `@problems` is a
/// best-effort surface, not a build gate.
fn run_cargo_check(root: &Path) -> Vec<Diagnostic> {
    let Some(stdout) =
        run_capture("cargo", &["check", "--message-format=json", "--quiet"], root)
    else {
        return Vec::new();
    };

    let mut diags = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
            continue;
        }
        let msg = match value.get("message") {
            Some(m) => m,
            None => continue,
        };
        let severity = msg
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("error")
            .to_string();
        let message = msg
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if message.is_empty() {
            continue;
        }
        let (path, line) = msg
            .get("spans")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.iter().find(|s| s.get("is_primary").and_then(|p| p.as_bool()) == Some(true)).or_else(|| arr.first()))
            .map(|span| {
                let p = span
                    .get("file_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let l = span
                    .get("line_start")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                (p, l)
            })
            .unwrap_or_default();
        diags.push(Diagnostic {
            source: "cargo".into(),
            severity,
            path,
            line,
            message,
        });
        if diags.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
    diags
}

/// Run `npx tsc --noEmit` and parse its line-oriented output. Each
/// diagnostic line looks like:
///   `path/to/file.ts(12,34): error TS2304: Cannot find name 'foo'.`
fn run_tsc(root: &Path) -> Vec<Diagnostic> {
    // tsc writes diagnostics to stdout (not stderr) when `--pretty false`.
    // `--no-install` keeps npx from fetching/installing a package on the fly;
    // run_capture bounds the wall-clock time so a hung/runaway tsc can't wedge
    // the caller.
    let Some(stdout) = run_capture(
        "npx",
        &["--no-install", "tsc", "--noEmit", "--pretty", "false"],
        root,
    ) else {
        return Vec::new();
    };
    parse_tsc_output(&stdout)
}

/// Parse the textual output of `tsc --noEmit --pretty false`. Extracted so
/// we can unit-test it without invoking the compiler.
fn parse_tsc_output(stdout: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Find the `(L,C):` segment.
        let Some(paren) = line.find('(') else {
            continue;
        };
        let after_paren = &line[paren + 1..];
        let Some(close) = after_paren.find(')') else {
            continue;
        };
        let coords = &after_paren[..close];
        let rest = &after_paren[close + 1..];
        // Strip the leading `: ` after `)`.
        let rest = rest.trim_start_matches(':').trim_start();
        let line_no = coords
            .split(',')
            .next()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        // `error TSxxxx: <message>` or `warning TSxxxx: <message>`.
        let (sev, message) = if let Some(stripped) = rest.strip_prefix("error ") {
            ("error", strip_ts_code(stripped))
        } else if let Some(stripped) = rest.strip_prefix("warning ") {
            ("warning", strip_ts_code(stripped))
        } else {
            ("error", rest.to_string())
        };
        if message.is_empty() {
            continue;
        }
        diags.push(Diagnostic {
            source: "tsc".into(),
            severity: sev.into(),
            path: line[..paren].to_string(),
            line: line_no,
            message,
        });
        if diags.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
    diags
}

/// `TS2304: Cannot find name 'foo'` → `Cannot find name 'foo'`.
fn strip_ts_code(s: &str) -> String {
    if let Some(idx) = s.find(": ") {
        let prefix = &s[..idx];
        if prefix.starts_with("TS") && prefix[2..].chars().all(|c| c.is_ascii_digit()) {
            return s[idx + 2..].trim().to_string();
        }
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsc_error_line() {
        let out = "src/foo.ts(12,5): error TS2304: Cannot find name 'foo'.";
        let diags = parse_tsc_output(out);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path, "src/foo.ts");
        assert_eq!(diags[0].line, 12);
        assert_eq!(diags[0].severity, "error");
        assert!(diags[0].message.contains("Cannot find name"));
    }

    #[test]
    fn skips_garbage_lines() {
        let out = "not a diagnostic\nsrc/x.ts(1,1): error TS1: nope.\nmore garbage";
        let diags = parse_tsc_output(out);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path, "src/x.ts");
    }

    #[test]
    fn ts_code_stripped_correctly() {
        assert_eq!(strip_ts_code("TS2304: oops"), "oops");
        assert_eq!(strip_ts_code("oops"), "oops");
    }

    #[test]
    fn collect_handles_empty_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let diags = collect(tmp.path());
        assert!(diags.is_empty());
    }
}
