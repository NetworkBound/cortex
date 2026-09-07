//! PRP gate runner — best-effort execution of the five quality gates against
//! a project root. Each gate ends up as one of:
//!
//!   * `pass`    — the command exited 0
//!   * `fail`    — the command exited non-zero (message is the tail of stderr)
//!   * `skipped` — the command isn't applicable (tool not installed, no
//!                 lockfile / Cargo.toml / etc.)
//!
//! We deliberately keep this thin: a real-world PRP harness would shell out to
//! a project-specific script, but the v1 wiring here exists so the panel can
//! show coloured pills today and we can iterate on what each gate actually
//! checks later.
//!
//! Output is always written back to the PRP file (via `update_prp_gates`) so
//! the next `list_prps` reflects the latest run.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::prp::loader::{update_prp_gates, GateStatuses, Prp};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GateVerdict {
    Pass,
    Fail,
    Skipped,
}

impl GateVerdict {
    fn as_str(&self) -> &'static str {
        match self {
            GateVerdict::Pass => "pass",
            GateVerdict::Fail => "fail",
            GateVerdict::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub name: String,
    pub verdict: GateVerdict,
    pub message: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub prp_name: String,
    pub gates: Vec<GateResult>,
}

/// Cap each gate to 60s so a hung subcommand doesn't wedge the UI thread.
/// `std::process::Command` doesn't support timeouts directly — we spawn the
/// child and poll `try_wait` on a tight loop. Worst-case kill on overrun.
fn run_with_timeout(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> (Option<i32>, String) {
    let mut child = match crate::sys::no_window(cmd)
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (None, format!("spawn failed: {e}")),
    };

    // Drain stdout/stderr concurrently on dedicated threads. The child can
    // produce more output than the OS pipe buffer holds (~64 KiB); if we only
    // read after the process exits, the child blocks writing to a full pipe and
    // never exits, wedging us until the timeout fires. Reading on separate
    // threads keeps both pipes flowing during execution.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = stdout_pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = stderr_pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Pipes hit EOF when the child exits, so the reader threads are
                // either done or about to be; join them to collect all output.
                let stdout = stdout_handle
                    .and_then(|h| h.join().ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let stderr = stderr_handle
                    .and_then(|h| h.join().ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let combined = if stderr.is_empty() { stdout } else { stderr };
                return (status.code(), tail(&combined, 480));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    // Reap the killed child so it doesn't linger as a zombie
                    // (and to release its pipe fds); kill() only sends the
                    // signal, wait() collects the exit status.
                    let _ = child.wait();
                    // Killing closes the pipes, so the reader threads unblock;
                    // join them so we don't leak threads on overrun.
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }
                    return (None, format!("timeout after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return (None, format!("wait failed: {e}")),
        }
    }
}

fn tail(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let skip = trimmed.chars().count() - max_chars;
    let suffix: String = trimmed.chars().skip(skip).collect();
    format!("…{suffix}")
}

/// Detect what kind of project we're sitting in so we can pick reasonable
/// commands per gate. Cheap repeated stat calls — fine for v1.
struct ProjectKind {
    has_cargo: bool,
    has_package_json: bool,
}

fn detect(root: &Path) -> ProjectKind {
    ProjectKind {
        has_cargo: root.join("Cargo.toml").is_file(),
        has_package_json: root.join("package.json").is_file(),
    }
}

fn which(bin: &str) -> bool {
    crate::sys::no_window("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn timed<F: FnOnce() -> (GateVerdict, String)>(name: &str, f: F) -> GateResult {
    let start = std::time::Instant::now();
    let (verdict, message) = f();
    GateResult {
        name: name.to_string(),
        verdict,
        message,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

fn gate_from_exit(name: &str, code: Option<i32>, msg: String) -> GateResult {
    let verdict = match code {
        Some(0) => GateVerdict::Pass,
        Some(_) => GateVerdict::Fail,
        None => GateVerdict::Fail,
    };
    GateResult {
        name: name.to_string(),
        verdict,
        message: msg,
        duration_ms: 0,
    }
}

fn syntax_gate(root: &Path, kind: &ProjectKind) -> GateResult {
    timed("syntax", || {
        if kind.has_cargo && which("cargo") {
            let (code, msg) = run_with_timeout(
                "cargo",
                &["check", "--quiet"],
                root,
                Duration::from_secs(60),
            );
            let g = gate_from_exit("syntax", code, msg);
            return (g.verdict, g.message);
        }
        if kind.has_package_json && which("npx") {
            let (code, msg) =
                run_with_timeout("npx", &["tsc", "--noEmit"], root, Duration::from_secs(60));
            let g = gate_from_exit("syntax", code, msg);
            return (g.verdict, g.message);
        }
        (GateVerdict::Skipped, "no cargo / tsc available".into())
    })
}

fn tests_gate(root: &Path, kind: &ProjectKind) -> GateResult {
    timed("tests", || {
        if kind.has_cargo && which("cargo") {
            let (code, msg) = run_with_timeout(
                "cargo",
                &["test", "--quiet", "--no-run"],
                root,
                Duration::from_secs(60),
            );
            let g = gate_from_exit("tests", code, msg);
            return (g.verdict, g.message);
        }
        if kind.has_package_json && which("npm") {
            let (code, msg) =
                run_with_timeout("npm", &["test", "--silent"], root, Duration::from_secs(60));
            let g = gate_from_exit("tests", code, msg);
            return (g.verdict, g.message);
        }
        (GateVerdict::Skipped, "no test runner available".into())
    })
}

fn coverage_gate(_root: &Path, _kind: &ProjectKind) -> GateResult {
    // No coverage harness wired in v1 — declare skipped explicitly so the
    // pill renders neutrally rather than red.
    timed("coverage", || {
        (
            GateVerdict::Skipped,
            "coverage runner not configured yet".into(),
        )
    })
}

fn build_gate(root: &Path, kind: &ProjectKind) -> GateResult {
    timed("build", || {
        if kind.has_cargo && which("cargo") {
            let (code, msg) = run_with_timeout(
                "cargo",
                &["build", "--quiet"],
                root,
                Duration::from_secs(60),
            );
            let g = gate_from_exit("build", code, msg);
            return (g.verdict, g.message);
        }
        if kind.has_package_json && which("npm") {
            let (code, msg) = run_with_timeout(
                "npm",
                &["run", "build", "--silent"],
                root,
                Duration::from_secs(60),
            );
            let g = gate_from_exit("build", code, msg);
            return (g.verdict, g.message);
        }
        (GateVerdict::Skipped, "no build target detected".into())
    })
}

fn security_gate(root: &Path, kind: &ProjectKind) -> GateResult {
    timed("security", || {
        if kind.has_cargo && which("cargo") && which("cargo-audit") {
            let (code, msg) = run_with_timeout(
                "cargo",
                &["audit", "--quiet"],
                root,
                Duration::from_secs(60),
            );
            let g = gate_from_exit("security", code, msg);
            return (g.verdict, g.message);
        }
        if kind.has_package_json && which("npm") {
            // `npm audit` returns non-zero when vulns exist — we treat that as
            // fail so the user notices. But it ALSO exits non-zero on registry
            // / network errors (offline, ENETUNREACH, ECONNREFUSED, registry
            // 5xx, etc.), which is not a security verdict. With `--json` those
            // surface as an `"error"` object in the output, so detect that and
            // skip rather than reporting a false security failure.
            let (code, msg) = run_with_timeout(
                "npm",
                &["audit", "--audit-level=high", "--json"],
                root,
                Duration::from_secs(60),
            );
            if code != Some(0) && msg.contains("\"error\"") {
                return (
                    GateVerdict::Skipped,
                    format!("npm audit unavailable (network/registry error): {msg}"),
                );
            }
            let g = gate_from_exit("security", code, msg);
            return (g.verdict, g.message);
        }
        (GateVerdict::Skipped, "no audit tool detected".into())
    })
}

/// Run all five gates against the project root and write the verdicts back
/// onto the PRP file. The returned report is what the UI renders.
pub fn run_gates(project_root: &Path, prp: &Prp) -> ValidationReport {
    let kind = detect(project_root);
    let results = vec![
        syntax_gate(project_root, &kind),
        tests_gate(project_root, &kind),
        coverage_gate(project_root, &kind),
        build_gate(project_root, &kind),
        security_gate(project_root, &kind),
    ];

    // Mirror into the PRP frontmatter so a later `list_prps` shows the new
    // verdicts. Best-effort — a write failure here doesn't sink the report.
    let mut gates: GateStatuses = GateStatuses::new();
    for r in &results {
        gates.insert(r.name.clone(), r.verdict.as_str().to_string());
    }
    let _ = update_prp_gates(project_root, &prp.name, gates);

    ValidationReport {
        prp_name: prp.name.clone(),
        gates: results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_suffix() {
        let s = "x".repeat(1000);
        let t = tail(&s, 50);
        assert!(t.starts_with('…'));
        assert_eq!(t.chars().count(), 51); // 1 ellipsis + 50 chars
    }

    #[test]
    fn tail_passthrough_short() {
        assert_eq!(tail("hi", 100), "hi");
    }

    #[test]
    fn gate_from_exit_maps_codes() {
        assert_eq!(
            gate_from_exit("x", Some(0), "".into()).verdict,
            GateVerdict::Pass
        );
        assert_eq!(
            gate_from_exit("x", Some(1), "".into()).verdict,
            GateVerdict::Fail
        );
        assert_eq!(
            gate_from_exit("x", None, "".into()).verdict,
            GateVerdict::Fail
        );
    }
}
