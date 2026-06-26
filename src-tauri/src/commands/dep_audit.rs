//! Dependency vulnerability audit. Detects ecosystem (npm / cargo / pip) by
//! sniffing the manifest in `project_root`, shells out to the matching audit
//! tool, parses its JSON, caps at 100 entries, and returns a normalised
//! report. Missing audit binaries surface as graceful error strings instead
//! of panics so the modal can show a helpful "install X" prompt.
//!
//! User flow: `/audit-deps` (alias `/vuln`). See `src/lib/dep-audit.ts` for
//! the TS wrapper and `src/components/DepAuditModal.tsx` for the modal.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;
use tokio::task;

use crate::app_state::AppState;

/// Hard cap on entries in the response. npm audit on a stale lockfile can
/// dump hundreds; the UI only ever pages through them so we lop the tail.
const MAX_ENTRIES: usize = 100;

/// Cap on the tail of the raw tool output we echo back. Useful for
/// debugging "why are there zero entries?" without flooding the frontend.
const RAW_TAIL_BYTES: usize = 4 * 1024;

#[derive(Debug, Serialize, Clone)]
pub struct Vulnerability {
    pub package: String,
    pub version: String,
    pub severity: String,
    pub summary: String,
    pub cve: Option<String>,
    pub fix_available: Option<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct SeveritySummary {
    pub critical: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
    pub unknown: u32,
}

#[derive(Debug, Serialize)]
pub struct DepAuditReport {
    pub ecosystem: String,
    pub vulnerabilities: Vec<Vulnerability>,
    pub summary: SeveritySummary,
    pub total_count: usize,
    pub raw_output_tail: String,
}

#[tauri::command]
pub async fn audit_deps(
    project_root: String,
    _state: State<'_, AppState>,
) -> Result<DepAuditReport, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let ecosystem = detect_ecosystem(&root)
        .ok_or_else(|| "no supported manifest found (package.json / Cargo.toml / pyproject.toml)".to_string())?;

    let report = task::spawn_blocking(move || run_audit(&root, ecosystem))
        .await
        .map_err(|e| format!("join error: {e}"))??;
    Ok(report)
}

fn detect_ecosystem(root: &Path) -> Option<&'static str> {
    // Order matters: a polyglot repo (e.g. Tauri) has both Cargo.toml and
    // package.json. We bias toward npm first because the Cortex repo itself
    // demonstrates the common case (`package.json` at the workspace root,
    // Cargo.toml under `src-tauri/`).
    if root.join("package.json").is_file() {
        return Some("npm");
    }
    if root.join("Cargo.toml").is_file() {
        return Some("cargo");
    }
    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        return Some("pip");
    }
    None
}

fn run_audit(root: &Path, ecosystem: &'static str) -> Result<DepAuditReport, String> {
    let (program, args): (&str, &[&str]) = match ecosystem {
        "npm" => ("npm", &["audit", "--json"]),
        "cargo" => ("cargo", &["audit", "--json"]),
        "pip" => ("pip-audit", &["--format", "json"]),
        other => return Err(format!("unsupported ecosystem: {other}")),
    };

    let output = match crate::sys::no_window(program).args(args).current_dir(root).output() {
        Ok(o) => o,
        Err(e) => {
            return Err(match e.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "{program} not installed — `{ecosystem}` audit needs `{program}` on PATH"
                ),
                _ => format!("spawn {program} failed: {e}"),
            })
        }
    };

    // npm audit exits non-zero when vulnerabilities are present — that's
    // expected, not an error. cargo-audit + pip-audit follow the same
    // convention. We rely on the stdout being JSON; stderr is captured only
    // for the raw_output_tail.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let raw = if stdout.is_empty() { stderr.clone() } else { stdout.clone() };
    let raw_output_tail = tail(&raw, RAW_TAIL_BYTES);

    // If we got nothing parseable AND the exit code is nonzero, treat that
    // as an outright failure so the modal can surface stderr.
    if stdout.trim().is_empty() && !output.status.success() {
        return Err(format!(
            "{program} exited {} with no JSON output:\n{}",
            output.status.code().unwrap_or(-1),
            tail(&stderr, 1024)
        ));
    }

    let mut vulns = match ecosystem {
        "npm" => parse_npm(&stdout),
        "cargo" => parse_cargo(&stdout),
        "pip" => parse_pip(&stdout),
        _ => Vec::new(),
    };

    let total_count = vulns.len();
    if vulns.len() > MAX_ENTRIES {
        vulns.truncate(MAX_ENTRIES);
    }
    let summary = tally(&vulns);

    Ok(DepAuditReport {
        ecosystem: ecosystem.to_string(),
        vulnerabilities: vulns,
        summary,
        total_count,
        raw_output_tail,
    })
}

fn tally(vulns: &[Vulnerability]) -> SeveritySummary {
    let mut s = SeveritySummary::default();
    for v in vulns {
        match normalize_severity(&v.severity).as_str() {
            "critical" => s.critical += 1,
            "high" => s.high += 1,
            "medium" => s.medium += 1,
            "low" => s.low += 1,
            _ => s.unknown += 1,
        }
    }
    s
}

/// Fold provider-specific severity tokens into the 5-bucket UI scheme.
pub fn normalize_severity(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "critical" => "critical".into(),
        "high" | "important" => "high".into(),
        "moderate" | "medium" | "warning" => "medium".into(),
        "low" | "info" | "informational" => "low".into(),
        _ => "unknown".into(),
    }
}

fn tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut cut = s.len() - limit;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[…truncated head…]\n{}", &s[cut..])
}

// ---- npm: `{ vulnerabilities: { <name>: { severity, via:[…], fixAvailable } } }`
// — `via` entries are either dep names (strings) or advisory objects with
// `title`/`cwe`; we pull the first object found for the user-facing summary.
fn parse_npm(stdout: &str) -> Vec<Vulnerability> {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Some(map) = v.get("vulnerabilities").and_then(|m| m.as_object()) else {
        return out;
    };
    for (name, entry) in map {
        let severity = entry
            .get("severity")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();
        let version = entry
            .get("range")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let mut summary = String::new();
        let mut cve: Option<String> = None;
        if let Some(via) = entry.get("via").and_then(|v| v.as_array()) {
            for item in via {
                if let Some(obj) = item.as_object() {
                    if summary.is_empty() {
                        if let Some(t) = obj.get("title").and_then(|s| s.as_str()) {
                            summary = t.to_string();
                        }
                    }
                    if cve.is_none() {
                        if let Some(cves) = obj.get("cwe").and_then(|c| c.as_array()) {
                            if let Some(first) = cves.first().and_then(|s| s.as_str()) {
                                cve = Some(first.to_string());
                            }
                        }
                    }
                }
            }
        }
        if summary.is_empty() {
            summary = format!("Vulnerability in {name}");
        }
        let fix_available = match entry.get("fixAvailable") {
            Some(serde_json::Value::Bool(true)) => Some("yes (npm audit fix)".to_string()),
            Some(serde_json::Value::Object(o)) => o
                .get("version")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        out.push(Vulnerability {
            package: name.clone(),
            version,
            severity,
            summary,
            cve,
            fix_available,
        });
    }
    out
}

// ---- cargo: `{ vulnerabilities: { list: [{ package: {name,version}, advisory:
// {id,title,severity}, versions: { patched:[…] } }] } }`.
fn parse_cargo(stdout: &str) -> Vec<Vulnerability> {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(list) = v
        .get("vulnerabilities")
        .and_then(|x| x.get("list"))
        .and_then(|x| x.as_array())
    else {
        return Vec::new();
    };
    list.iter()
        .map(|entry| {
            let package = entry
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|s| s.as_str())
                .unwrap_or("?")
                .to_string();
            let version = entry
                .get("package")
                .and_then(|p| p.get("version"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let advisory = entry.get("advisory");
            let summary = advisory
                .and_then(|a| a.get("title"))
                .and_then(|s| s.as_str())
                .unwrap_or("Vulnerability")
                .to_string();
            let cve = advisory
                .and_then(|a| a.get("id"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let severity = advisory
                .and_then(|a| a.get("severity"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let fix_available = entry
                .get("versions")
                .and_then(|v| v.get("patched"))
                .and_then(|x| x.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            Vulnerability {
                package,
                version,
                severity,
                summary,
                cve,
                fix_available,
            }
        })
        .collect()
}

// ---- pip: `{ dependencies: [{ name, version, vulns: [{ id, fix_versions,
// description }] }] }`. Severity isn't always present — pip-audit defers to
// the underlying advisory DB — so we mark unknown and use description as
// the summary.
fn parse_pip(stdout: &str) -> Vec<Vulnerability> {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Some(deps) = v.get("dependencies").and_then(|d| d.as_array()) else {
        return out;
    };
    for dep in deps {
        let name = dep
            .get("name")
            .and_then(|s| s.as_str())
            .unwrap_or("?")
            .to_string();
        let version = dep
            .get("version")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(vulns) = dep.get("vulns").and_then(|v| v.as_array()) {
            for vuln in vulns {
                let summary = vuln
                    .get("description")
                    .and_then(|s| s.as_str())
                    .or_else(|| vuln.get("id").and_then(|s| s.as_str()))
                    .unwrap_or("Vulnerability")
                    .to_string();
                let cve = vuln
                    .get("id")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let fix_available = vuln
                    .get("fix_versions")
                    .and_then(|x| x.as_array())
                    .and_then(|a| a.first())
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                // pip-audit doesn't ship a severity field directly; some
                // advisory feeds inject it under `aliases` / `severity`. Use
                // it when present, otherwise mark unknown.
                let severity = vuln
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                out.push(Vulnerability {
                    package: name.clone(),
                    version: version.clone(),
                    severity,
                    summary,
                    cve,
                    fix_available,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_severity_buckets_known_values() {
        assert_eq!(normalize_severity("CRITICAL"), "critical");
        assert_eq!(normalize_severity("High"), "high");
        assert_eq!(normalize_severity("important"), "high");
        assert_eq!(normalize_severity("Moderate"), "medium");
        assert_eq!(normalize_severity("info"), "low");
        assert_eq!(normalize_severity("zzz"), "unknown");
    }

    #[test]
    fn parse_npm_extracts_basics() {
        let blob = r#"{
          "vulnerabilities": {
            "lodash": {
              "name": "lodash",
              "severity": "high",
              "range": "<4.17.21",
              "via": [{"title":"Prototype Pollution","cwe":["CWE-1321"]}],
              "fixAvailable": true
            }
          }
        }"#;
        let out = parse_npm(blob);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package, "lodash");
        assert_eq!(out[0].severity, "high");
        assert!(out[0].summary.contains("Prototype"));
        assert_eq!(out[0].cve.as_deref(), Some("CWE-1321"));
        assert!(out[0].fix_available.is_some());
    }

    #[test]
    fn parse_cargo_extracts_basics() {
        let blob = r#"{
          "vulnerabilities": {
            "list": [{
              "package": {"name":"foo","version":"0.1.0"},
              "advisory": {"id":"RUSTSEC-2024-0001","title":"foo bug","severity":"high"},
              "versions": {"patched":["^0.2"]}
            }]
          }
        }"#;
        let out = parse_cargo(blob);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package, "foo");
        assert_eq!(out[0].cve.as_deref(), Some("RUSTSEC-2024-0001"));
        assert_eq!(out[0].fix_available.as_deref(), Some("^0.2"));
    }

    #[test]
    fn parse_pip_extracts_basics() {
        let blob = r#"{
          "dependencies": [
            {"name":"requests","version":"2.0.0","vulns":[
              {"id":"GHSA-xxxx","description":"requests bug","fix_versions":["2.31.0"]}
            ]}
          ]
        }"#;
        let out = parse_pip(blob);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package, "requests");
        assert_eq!(out[0].cve.as_deref(), Some("GHSA-xxxx"));
        assert_eq!(out[0].fix_available.as_deref(), Some("2.31.0"));
    }

    #[test]
    fn tally_counts_per_bucket() {
        let v = vec![
            Vulnerability {
                package: "a".into(),
                version: "1".into(),
                severity: "critical".into(),
                summary: "".into(),
                cve: None,
                fix_available: None,
            },
            Vulnerability {
                package: "b".into(),
                version: "1".into(),
                severity: "high".into(),
                summary: "".into(),
                cve: None,
                fix_available: None,
            },
            Vulnerability {
                package: "c".into(),
                version: "1".into(),
                severity: "moderate".into(),
                summary: "".into(),
                cve: None,
                fix_available: None,
            },
            Vulnerability {
                package: "d".into(),
                version: "1".into(),
                severity: "info".into(),
                summary: "".into(),
                cve: None,
                fix_available: None,
            },
        ];
        let s = tally(&v);
        assert_eq!(s.critical, 1);
        assert_eq!(s.high, 1);
        assert_eq!(s.medium, 1);
        assert_eq!(s.low, 1);
    }

    #[test]
    fn tail_keeps_tail_when_too_long() {
        let s = "a".repeat(2000);
        let t = tail(&s, 500);
        assert!(t.starts_with("[…truncated"));
        assert!(t.len() < s.len());
    }
}
