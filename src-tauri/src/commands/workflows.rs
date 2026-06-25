//! Workflow templates — pre-canned multi-step recipes the user can launch
//! in one click. Each workflow is a YAML file at
//! `~/.cortex/workflows/<name>.yaml` describing an ordered list of steps;
//! every step has a `role` (which agent persona to use) and a `prompt`.
//!
//! On-disk schema:
//! ```yaml
//! name: review-pr
//! description: Run reviewer + auditor + tester on the active PR
//! steps:
//!   - role: code-reviewer
//!     prompt: "Review the current branch diff for correctness bugs."
//!   - role: security-auditor
//!     prompt: "Audit the diff for injection, secret leaks, missing input validation."
//! ```
//!
//! For v1, `run_workflow` is fire-and-forget: it returns a run id + the
//! expanded step list, and the frontend takes care of dispatching one chat
//! message per step. That keeps us off the orchestrator critical path and
//! avoids fighting the existing chat pipeline for stream ordering.
//!
//! Seeded defaults (`review-pr`, `morning-standup`, `triage-bug`,
//! `prep-release`, `audit-deps`) land on first run only. Once the
//! directory exists we never re-seed, so a user can delete a default and
//! it stays gone.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single step in a workflow. `role` is the persona name (matches a file
/// under `~/.cortex/roles/`); `prompt` is the body the chat pipeline sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub role: String,
    pub prompt: String,
}

/// A full workflow as exposed to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
}

/// On-disk form — `name` is optional so the filename can supply it.
#[derive(Debug, Deserialize)]
struct WorkflowFile {
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    steps: Vec<WorkflowStep>,
}

/// Returned from `run_workflow`. The frontend uses `steps` to drive its
/// sequential chat dispatch; `run_id` is a short unique tag for tracing.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    pub started_unix_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn workflows_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("workflows"))
}

/// Reject names with path separators / `..` so callers can't escape the
/// workflows dir. Mirrors the rule used in `agents/roles.rs`.
fn is_safe_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains("..")
}

fn parse_workflow(raw: &str, fallback_name: &str) -> anyhow::Result<Workflow> {
    let parsed: WorkflowFile = serde_yaml::from_str(raw)?;
    Ok(Workflow {
        name: parsed.name.unwrap_or_else(|| fallback_name.to_string()),
        description: parsed.description,
        steps: parsed.steps,
    })
}

fn read_all() -> Vec<Workflow> {
    let Some(dir) = workflows_dir() else { return Vec::new() };
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("workflows: no dir ({}): {e}", dir.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|s| s.to_str()) {
            Some("yaml") | Some("yml") => {}
            _ => continue,
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("workflows: read failed for {}: {e}", path.display());
                continue;
            }
        };
        match parse_workflow(&raw, &stem) {
            Ok(w) => out.push(w),
            Err(e) => tracing::debug!(
                "workflows: parse failed for {}: {e}",
                path.display()
            ),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn read_one(name: &str) -> Option<Workflow> {
    if !is_safe_name(name) {
        return None;
    }
    let dir = workflows_dir()?;
    for ext in ["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if let Ok(raw) = fs::read_to_string(&path) {
            match parse_workflow(&raw, name) {
                Ok(w) => return Some(w),
                Err(e) => {
                    tracing::debug!(
                        "workflows: parse failed for {}: {e}",
                        path.display()
                    );
                    return None;
                }
            }
        }
    }
    None
}

fn write_one(workflow: &Workflow) -> anyhow::Result<()> {
    if !is_safe_name(&workflow.name) {
        anyhow::bail!("invalid workflow name '{}'", workflow.name);
    }
    let dir = workflows_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.yaml", workflow.name));
    let body = serde_yaml::to_string(workflow)?;
    fs::write(&path, body)?;
    Ok(())
}

fn remove_one(name: &str) -> anyhow::Result<()> {
    if !is_safe_name(name) {
        anyhow::bail!("invalid workflow name '{name}'");
    }
    let Some(dir) = workflows_dir() else {
        return Ok(());
    };
    for ext in ["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Five preset workflows seeded on first launch. Only seeds when the
/// `~/.cortex/workflows/` directory does NOT yet exist, so deletions stick.
pub fn seed_default_workflows() {
    let Some(dir) = workflows_dir() else { return };
    if dir.exists() {
        return;
    }
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::debug!(
            "workflows: seed mkdir failed at {}: {e}",
            dir.display()
        );
        return;
    }
    for wf in default_workflows() {
        if let Err(e) = write_one(&wf) {
            tracing::debug!("workflows: seed write failed for {}: {e}", wf.name);
        }
    }
}

fn default_workflows() -> Vec<Workflow> {
    vec![
        Workflow {
            name: "review-pr".into(),
            description: Some(
                "Run code-reviewer + security-auditor + test-writer on the active PR"
                    .into(),
            ),
            steps: vec![
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Review the current branch diff for correctness bugs. \
                             Focus on off-by-one, null deref, race conditions, and \
                             error-handling gaps. Quote line numbers."
                        .into(),
                },
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Audit the diff for injection (SQL, command, prompt), \
                             secret leaks, broken authn/authz, and missing input \
                             validation. Mark findings 'suspected' vs 'confirmed'."
                        .into(),
                },
                WorkflowStep {
                    role: "test-writer".into(),
                    prompt: "Generate vitest/cargo tests for the new public \
                             functions in the diff. Cover happy path plus at least \
                             two error cases per function."
                        .into(),
                },
            ],
        },
        Workflow {
            name: "morning-standup".into(),
            description: Some(
                "Summarize yesterday's work, pull open PRs, list today's priorities"
                    .into(),
            ),
            steps: vec![
                WorkflowStep {
                    role: "bug-triager".into(),
                    prompt: "Summarize what changed in this repo since yesterday: \
                             commits, merged PRs, and any failing CI runs. Two \
                             sentences max per item."
                        .into(),
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "List open pull requests in this repo with a one-line \
                             status (waiting for review / changes requested / \
                             ready to merge)."
                        .into(),
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Based on the active focus chain and recent commits, \
                             draft a 3-bullet plan for today's work."
                        .into(),
                },
            ],
        },
        Workflow {
            name: "triage-bug".into(),
            description: Some(
                "Reproduce, isolate, and propose a fix for the bug currently in the chat"
                    .into(),
            ),
            steps: vec![
                WorkflowStep {
                    role: "bug-triager".into(),
                    prompt: "Reproduce the bug described above. List the exact \
                             steps, the failing input, and the observed vs \
                             expected output."
                        .into(),
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Identify the offending function / commit. Show the \
                             smallest patch that would fix it without regressing \
                             nearby behavior."
                        .into(),
                },
                WorkflowStep {
                    role: "test-writer".into(),
                    prompt: "Write a regression test that would have caught this \
                             bug. Use the existing test framework."
                        .into(),
                },
            ],
        },
        Workflow {
            name: "prep-release".into(),
            description: Some(
                "Generate changelog, bump version, audit deps before cutting a release"
                    .into(),
            ),
            steps: vec![
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Draft a CHANGELOG entry for the next release. Group \
                             commits since the last tag into Features / Fixes / \
                             Internal. Skip noise."
                        .into(),
                },
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Audit dependency changes since the last release. \
                             Flag anything with a CVE, a major version bump, or a \
                             new transitive dep from an unfamiliar publisher."
                        .into(),
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Suggest the next semantic version (patch / minor / \
                             major) based on the diff since the last tag. Justify \
                             in one sentence."
                        .into(),
                },
            ],
        },
        Workflow {
            name: "audit-deps".into(),
            description: Some(
                "Scan every direct dependency for vulns, abandoned crates, and license issues"
                    .into(),
            ),
            steps: vec![
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "List every direct dependency in this repo (npm + \
                             cargo). For each one, note: last release date, \
                             known CVEs, and license."
                        .into(),
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Flag any dependency that looks abandoned (no \
                             release in 18+ months) or duplicated by something \
                             already in the tree."
                        .into(),
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Summarize the audit as a 5-bullet action plan: \
                             which deps to upgrade, replace, or drop, in priority \
                             order."
                        .into(),
                },
            ],
        },
    ]
}

// ---------- Tauri commands ----------

/// List every workflow under `~/.cortex/workflows/*.yaml`, sorted by name.
#[tauri::command]
pub async fn list_workflows() -> Result<Vec<Workflow>, String> {
    tokio::task::spawn_blocking(read_all)
        .await
        .map_err(|e| format!("join error: {e}"))
}

/// Load a single workflow by filename stem.
#[tauri::command]
pub async fn get_workflow(name: String) -> Result<Workflow, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    tokio::task::spawn_blocking(move || read_one(&name).ok_or_else(|| "not found".to_string()))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Create or update a workflow on disk. Returns the workflow as persisted.
#[tauri::command]
pub async fn save_workflow(workflow: Workflow) -> Result<Workflow, String> {
    tokio::task::spawn_blocking(move || {
        write_one(&workflow).map_err(|e| e.to_string())?;
        Ok::<Workflow, String>(workflow)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Delete a workflow file. Missing files are a no-op.
#[tauri::command]
pub async fn delete_workflow(name: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    tokio::task::spawn_blocking(move || remove_one(&name).map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Resolve a workflow by name and return a `WorkflowRun` the frontend uses
/// to drive sequential chat dispatch. The backend does NOT enqueue chat
/// messages itself — keeps us off the streaming pipeline and avoids
/// re-ordering races with user input.
#[tauri::command]
pub async fn run_workflow(name: String) -> Result<WorkflowRun, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    tokio::task::spawn_blocking(move || {
        let wf = read_one(&name).ok_or_else(|| format!("workflow '{name}' not found"))?;
        if wf.steps.is_empty() {
            return Err(format!("workflow '{}' has no steps", wf.name));
        }
        let run_id = format!("wf-{}-{}", wf.name, now_ms());
        Ok(WorkflowRun {
            run_id,
            name: wf.name,
            steps: wf.steps,
            started_unix_ms: now_ms(),
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _g = LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn list_empty_when_dir_missing() {
        with_temp_home(|| {
            assert!(read_all().is_empty());
        });
    }

    #[test]
    fn seed_then_list() {
        with_temp_home(|| {
            seed_default_workflows();
            let listed = read_all();
            assert!(listed.iter().any(|w| w.name == "review-pr"));
            assert!(listed.iter().any(|w| w.name == "audit-deps"));
            // Delete a default and re-seed: it must NOT come back.
            remove_one("review-pr").unwrap();
            seed_default_workflows();
            let again = read_all();
            assert!(again.iter().all(|w| w.name != "review-pr"));
        });
    }

    #[test]
    fn set_then_get_then_delete() {
        with_temp_home(|| {
            let wf = Workflow {
                name: "demo".into(),
                description: Some("d".into()),
                steps: vec![WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "hi".into(),
                }],
            };
            write_one(&wf).unwrap();
            assert_eq!(read_one("demo").unwrap(), wf);
            remove_one("demo").unwrap();
            assert!(read_one("demo").is_none());
        });
    }

    #[test]
    fn rejects_path_traversal() {
        with_temp_home(|| {
            assert!(read_one("../etc/passwd").is_none());
            assert!(read_one("sub/dir").is_none());
            assert!(write_one(&Workflow {
                name: "../evil".into(),
                description: None,
                steps: vec![],
            })
            .is_err());
        });
    }

    #[test]
    fn name_falls_back_to_filename() {
        with_temp_home(|| {
            let dir = workflows_dir().unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("scratch.yaml"),
                "description: hi\nsteps:\n  - role: code-reviewer\n    prompt: x\n",
            )
            .unwrap();
            let w = read_one("scratch").unwrap();
            assert_eq!(w.name, "scratch");
            assert_eq!(w.steps.len(), 1);
        });
    }
}
