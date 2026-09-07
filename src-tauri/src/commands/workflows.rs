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
//! v2 adds three strictly-additive fields (old YAML parses and runs
//! byte-identically):
//! - `inputs`: declared `{{key}}` template values collected by the frontend
//!   before Run and expanded server-side. Expansion is exact-key only
//!   (`[a-z0-9_-]{1,32}`) — no expression language, no eval; values are plain
//!   text inserted into prompts the user already controls. Workflows that
//!   declare NO inputs skip expansion entirely, so a v1 prompt containing a
//!   literal `{{...}}` keeps running unchanged.
//! - per-step `model`: optional model override the frontend forwards through
//!   the existing chat routing (e.g. `ollama:...` or a gateway model id).
//! - per-step `pipe_output`: the frontend prefixes the NEXT step's prompt
//!   with this step's captured answer at dispatch time.
//!
//! Seeded defaults (`review-pr`, `morning-standup`, `triage-bug`,
//! `prep-release`, `audit-deps`, plus the five-recipe v2 catalog —
//! `audit-repo`, `fix-failing-tests`, `release-notes`, `pr-review`,
//! `security-scan-before-merge`) land on first run only. Once the
//! directory exists we never re-seed, so a user can delete a default and
//! it stays gone.
//!
//! Full-scope additions: [`recipe_catalog`] (five parameterized, per-step-
//! model recipes) and `export_workflow` / `import_workflow` (round-trip a
//! single workflow as a YAML file from the Workflows panel).

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// serde helper — lets `pipe_output: false` (the v1 default) stay off disk
/// and off the wire so v1 files and payloads round-trip byte-identically.
fn is_false(b: &bool) -> bool {
    !*b
}

/// A single step in a workflow. `role` is the persona name (matches a file
/// under `~/.cortex/roles/`); `prompt` is the body the chat pipeline sends.
/// `model` / `pipe_output` are v2 additions — absent in v1 files and skipped
/// on serialize when unset, so old YAML round-trips unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub role: String,
    pub prompt: String,
    /// Optional model override for this step (e.g. `ollama:llama3.1` or a
    /// gateway model id). Routed by the frontend through the existing chat
    /// pipeline — the backend never dispatches directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// When true, the frontend prefixes the NEXT step's prompt with this
    /// step's captured assistant output at dispatch time.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pipe_output: bool,
}

/// A declared template input. `{{key}}` occurrences in step prompts are
/// replaced server-side by `run_workflow` after validating required keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInput {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

/// A full workflow as exposed to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// v2: declared template inputs. Empty for every v1 file (serde default)
    /// and skipped on serialize, so v1 YAML round-trips byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<WorkflowInput>,
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
    inputs: Vec<WorkflowInput>,
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
    crate::paths::home_dir().map(|h| h.join(".cortex").join("workflows"))
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
        inputs: parsed.inputs,
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

/// Ten preset workflows seeded on first launch — the original five v1-shaped
/// defaults plus the five-recipe v2 catalog ([`recipe_catalog`]). Only seeds
/// when the `~/.cortex/workflows/` directory does NOT yet exist, so
/// deletions stick.
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
    for wf in default_workflows().into_iter().chain(recipe_catalog()) {
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
            inputs: vec![],
            steps: vec![
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Review the current branch diff for correctness bugs. \
                             Focus on off-by-one, null deref, race conditions, and \
                             error-handling gaps. Quote line numbers."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Audit the diff for injection (SQL, command, prompt), \
                             secret leaks, broken authn/authz, and missing input \
                             validation. Mark findings 'suspected' vs 'confirmed'."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "test-writer".into(),
                    prompt: "Generate vitest/cargo tests for the new public \
                             functions in the diff. Cover happy path plus at least \
                             two error cases per function."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "morning-standup".into(),
            description: Some(
                "Summarize yesterday's work, pull open PRs, list today's priorities"
                    .into(),
            ),
            inputs: vec![],
            steps: vec![
                WorkflowStep {
                    role: "bug-triager".into(),
                    prompt: "Summarize what changed in this repo since yesterday: \
                             commits, merged PRs, and any failing CI runs. Two \
                             sentences max per item."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "List open pull requests in this repo with a one-line \
                             status (waiting for review / changes requested / \
                             ready to merge)."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Based on the active focus chain and recent commits, \
                             draft a 3-bullet plan for today's work."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "triage-bug".into(),
            description: Some(
                "Reproduce, isolate, and propose a fix for the bug currently in the chat"
                    .into(),
            ),
            inputs: vec![],
            steps: vec![
                WorkflowStep {
                    role: "bug-triager".into(),
                    prompt: "Reproduce the bug described above. List the exact \
                             steps, the failing input, and the observed vs \
                             expected output."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Identify the offending function / commit. Show the \
                             smallest patch that would fix it without regressing \
                             nearby behavior."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "test-writer".into(),
                    prompt: "Write a regression test that would have caught this \
                             bug. Use the existing test framework."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "prep-release".into(),
            description: Some(
                "Generate changelog, bump version, audit deps before cutting a release"
                    .into(),
            ),
            inputs: vec![],
            steps: vec![
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Draft a CHANGELOG entry for the next release. Group \
                             commits since the last tag into Features / Fixes / \
                             Internal. Skip noise."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Audit dependency changes since the last release. \
                             Flag anything with a CVE, a major version bump, or a \
                             new transitive dep from an unfamiliar publisher."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Suggest the next semantic version (patch / minor / \
                             major) based on the diff since the last tag. Justify \
                             in one sentence."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "audit-deps".into(),
            description: Some(
                "Scan every direct dependency for vulns, abandoned crates, and license issues"
                    .into(),
            ),
            inputs: vec![],
            steps: vec![
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "List every direct dependency in this repo (npm + \
                             cargo). For each one, note: last release date, \
                             known CVEs, and license."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Flag any dependency that looks abandoned (no \
                             release in 18+ months) or duplicated by something \
                             already in the tree."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Summarize the audit as a 5-bullet action plan: \
                             which deps to upgrade, replace, or drop, in priority \
                             order."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
    ]
}

/// Full-scope recipe catalog: five ready-to-run workflows that exercise
/// every v2 field (`inputs`, per-step `model`, `pipe_output`) end to end.
/// Appended to [`default_workflows`] on first seed — strictly additive, the
/// original five v1-shaped defaults are untouched so `seed_then_list` and
/// every existing name keep working exactly as before.
///
/// Model policy: a step whose job is to *summarize/condense* prior output
/// pins a cheap local model (`ollama:llama3.2:1b` — small enough to be a
/// reasonable default pull, matching the id shape already used elsewhere,
/// e.g. `src/lib/e2e-probe.ts`'s `EVAL_REAL_MODEL`). A step whose job is to
/// *review/judge* is left unpinned (`model: None` → the existing chat
/// pipeline's default routing) rather than hard-coding a "strong" model id
/// that may not exist in a given user's gateway/CLI config — an explicit
/// user pick in the step editor always wins over this default.
fn recipe_catalog() -> Vec<Workflow> {
    vec![
        Workflow {
            name: "audit-repo".into(),
            description: Some(
                "Audit this repo — security + correctness pass, summarized into a \
                 ranked checklist"
                    .into(),
            ),
            inputs: vec![WorkflowInput {
                key: "scope".into(),
                label: Some("Scope (path or area)".into()),
                default: Some("the whole repo".into()),
                required: false,
            }],
            steps: vec![
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Audit {{scope}} for injection points, secret leaks, and \
                             broken authn/authz. Mark findings 'suspected' vs \
                             'confirmed'."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Building on the audit above, review {{scope}} for \
                             correctness bugs and race conditions. Quote line \
                             numbers."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Summarize both passes above into a single ranked \
                             checklist (Critical / High / Low)."
                        .into(),
                    model: Some("ollama:llama3.2:1b".into()),
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "fix-failing-tests".into(),
            description: Some(
                "Reproduce, isolate, and fix a failing test suite from pasted output"
                    .into(),
            ),
            inputs: vec![WorkflowInput {
                key: "test_output".into(),
                label: Some("Paste the failing test output".into()),
                default: None,
                required: true,
            }],
            steps: vec![
                WorkflowStep {
                    role: "bug-triager".into(),
                    prompt: "Here is the failing test output:\n\n{{test_output}}\n\n\
                             Isolate which test(s) are failing and why. Two \
                             sentences max per failure."
                        .into(),
                    model: Some("ollama:llama3.2:1b".into()),
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Given the triage above, propose the smallest patch \
                             that would fix the failure(s) without regressing \
                             nearby behavior."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "test-writer".into(),
                    prompt: "Write a regression test that would have caught this \
                             failure. Use the existing test framework."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "release-notes".into(),
            description: Some(
                "Draft release notes since a tag, then tighten them for accuracy"
                    .into(),
            ),
            inputs: vec![
                WorkflowInput {
                    key: "tag".into(),
                    label: Some("Previous release tag".into()),
                    default: None,
                    required: true,
                },
                WorkflowInput {
                    key: "tone".into(),
                    label: Some("Tone".into()),
                    default: Some("neutral".into()),
                    required: false,
                },
            ],
            steps: vec![
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Draft release notes since {{tag}} in a {{tone}} tone, \
                             grouped into Features / Fixes / Internal. Skip noise."
                        .into(),
                    model: Some("ollama:llama3.2:1b".into()),
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Tighten the draft above for clarity and accuracy. \
                             Flag anything that looks like a breaking change."
                        .into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "pr-review".into(),
            description: Some(
                "Review PR — reviewer + security pass, summarized into a merge verdict"
                    .into(),
            ),
            inputs: vec![WorkflowInput {
                key: "pr_ref".into(),
                label: Some("PR number or branch (optional)".into()),
                default: Some("the active branch".into()),
                required: false,
            }],
            steps: vec![
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Review {{pr_ref}} for correctness bugs, off-by-one \
                             errors, and error-handling gaps. Quote line numbers."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Building on the review above, audit {{pr_ref}} for \
                             injection, secret leaks, and missing input validation."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Summarize both passes above into a one-line merge \
                             verdict: approve, request changes, or block."
                        .into(),
                    model: Some("ollama:llama3.2:1b".into()),
                    pipe_output: false,
                },
            ],
        },
        Workflow {
            name: "security-scan-before-merge".into(),
            description: Some(
                "Security scan before merge — deps + diff, gated on confirmed findings"
                    .into(),
            ),
            inputs: vec![WorkflowInput {
                key: "pr_ref".into(),
                label: Some("PR number or branch (optional)".into()),
                default: Some("the active branch".into()),
                required: false,
            }],
            steps: vec![
                WorkflowStep {
                    role: "security-auditor".into(),
                    prompt: "Scan {{pr_ref}} and its dependency diff for CVEs, \
                             license issues, and abandoned crates. Mark findings \
                             'suspected' vs 'confirmed'."
                        .into(),
                    model: None,
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Given the scan above, call out anything that should \
                             block the merge vs. can land as a follow-up, in one \
                             short list."
                        .into(),
                    model: Some("ollama:llama3.2:1b".into()),
                    pipe_output: false,
                },
            ],
        },
    ]
}

// ---------- Template expansion (v2) ----------

/// Exact-key placeholder: `{{key}}` where key is `[a-z0-9_-]{1,32}`.
/// Anything else (`{{ key }}`, `{{KEY}}`, `{{a.b}}`, unbalanced braces) is
/// NOT a placeholder and passes through as literal text — there is no
/// expression language and no eval, by design.
static PLACEHOLDER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\{\{([a-z0-9_-]{1,32})\}\}").expect("static regex"));

/// True when `key` is usable as a template input key (same charset the
/// placeholder regex accepts).
fn is_valid_input_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Merge declared inputs with the values the caller provided. Rules:
/// - every declared key must match `[a-z0-9_-]{1,32}`;
/// - provided keys must be declared (no arbitrary interpolation);
/// - value precedence: provided, then `default`, then `""`;
/// - `required` inputs must end up non-blank.
fn resolve_inputs(
    declared: &[WorkflowInput],
    provided: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    for key in provided.keys() {
        if !declared.iter().any(|i| i.key == *key) {
            return Err(format!("input '{key}' is not declared by this workflow"));
        }
    }
    let mut values = HashMap::new();
    for input in declared {
        if !is_valid_input_key(&input.key) {
            return Err(format!(
                "invalid input key '{}' — use a-z, 0-9, _ or - (max 32 chars)",
                input.key
            ));
        }
        let value = provided
            .get(&input.key)
            .cloned()
            .or_else(|| input.default.clone())
            .unwrap_or_default();
        if input.required && value.trim().is_empty() {
            return Err(format!("missing required input '{}'", input.key));
        }
        values.insert(input.key.clone(), value);
    }
    Ok(values)
}

/// Replace every `{{key}}` placeholder in `prompt` with its resolved value.
/// A placeholder whose key is not in `values` (i.e. not declared) is an
/// error — exact-key expansion only, never a silent passthrough.
fn expand_prompt(prompt: &str, values: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(prompt.len());
    let mut last = 0usize;
    for caps in PLACEHOLDER_RE.captures_iter(prompt) {
        let whole = caps.get(0).expect("group 0 always present");
        let key = &caps[1];
        let Some(value) = values.get(key) else {
            return Err(format!(
                "unknown input '{{{{{key}}}}}' — declare it under `inputs`"
            ));
        };
        out.push_str(&prompt[last..whole.start()]);
        out.push_str(value);
        last = whole.end();
    }
    out.push_str(&prompt[last..]);
    Ok(out)
}

/// Resolve a workflow into a `WorkflowRun`. Workflows that declare no inputs
/// skip expansion entirely, so v1 files produce byte-identical steps.
/// Step order (and `model` / `pipe_output` flags) are preserved verbatim —
/// the frontend relies on that ordering to pipe step N's output into N+1.
fn build_run(wf: Workflow, provided: HashMap<String, String>) -> Result<WorkflowRun, String> {
    if wf.steps.is_empty() {
        return Err(format!("workflow '{}' has no steps", wf.name));
    }
    let steps = if wf.inputs.is_empty() {
        if !provided.is_empty() {
            return Err(format!("workflow '{}' declares no inputs", wf.name));
        }
        wf.steps
    } else {
        let values = resolve_inputs(&wf.inputs, &provided)?;
        wf.steps
            .into_iter()
            .map(|mut s| {
                s.prompt = expand_prompt(&s.prompt, &values)?;
                Ok(s)
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    let run_id = format!("wf-{}-{}", wf.name, now_ms());
    Ok(WorkflowRun {
        run_id,
        name: wf.name,
        steps,
        started_unix_ms: now_ms(),
    })
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

/// Serialize a workflow to the same YAML shape `write_one` persists —
/// v1-shaped workflows (no inputs, no per-step `model`/`pipe_output`)
/// serialize with none of the v2 keys present, so exporting and re-importing
/// an untouched v1 file round-trips byte-identically.
fn workflow_to_yaml(workflow: &Workflow) -> Result<String, String> {
    serde_yaml::to_string(workflow).map_err(|e| format!("failed to serialize workflow: {e}"))
}

/// Parse a workflow YAML document read from an arbitrary file (import).
/// Unlike `parse_workflow` used for on-disk `~/.cortex/workflows/*.yaml`
/// (trusted, already-safe filenames), this also re-validates the resulting
/// name so an imported file can't smuggle a path-traversal name into
/// `write_one` with a confusing low-level error.
fn import_from_text(raw: &str, fallback_name: &str) -> Result<Workflow, String> {
    let wf = parse_workflow(raw, fallback_name).map_err(|e| format!("invalid workflow YAML: {e}"))?;
    if !is_safe_name(&wf.name) {
        return Err(format!("invalid workflow name '{}'", wf.name));
    }
    if wf.steps.is_empty() {
        return Err(format!("workflow '{}' has no steps", wf.name));
    }
    Ok(wf)
}

/// Export a workflow as a YAML string (frontend writes it to a user-chosen
/// path via the file-save dialog). Read-only — does not touch disk state.
#[tauri::command]
pub async fn export_workflow(name: String) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    tokio::task::spawn_blocking(move || {
        let wf = read_one(&name).ok_or_else(|| format!("workflow '{name}' not found"))?;
        workflow_to_yaml(&wf)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Import a workflow from a YAML file at an arbitrary path (the frontend
/// resolves `path` via the native file-open dialog). Refuses to clobber an
/// existing workflow of the same name — the caller deletes/renames first,
/// same "no silent overwrite" rule the panel's rename-collision check uses.
#[tauri::command]
pub async fn import_workflow(path: String) -> Result<Workflow, String> {
    if path.trim().is_empty() {
        return Err("path is required".into());
    }
    tokio::task::spawn_blocking(move || {
        let raw = fs::read_to_string(&path).map_err(|e| format!("failed to read '{path}': {e}"))?;
        let stem = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported")
            .to_string();
        let wf = import_from_text(&raw, &stem)?;
        if read_one(&wf.name).is_some() {
            return Err(format!(
                "a workflow named '{}' already exists — rename it in the file or delete the existing one first",
                wf.name
            ));
        }
        write_one(&wf).map_err(|e| e.to_string())?;
        Ok(wf)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Resolve a workflow by name and return a `WorkflowRun` the frontend uses
/// to drive sequential chat dispatch. The backend does NOT enqueue chat
/// messages itself — keeps us off the streaming pipeline and avoids
/// re-ordering races with user input.
///
/// v2: `inputs` (optional, so v1 callers are unaffected) carries the values
/// the frontend collected for the workflow's declared `inputs`; required
/// keys are validated and `{{key}}` placeholders expanded server-side.
#[tauri::command]
pub async fn run_workflow(
    name: String,
    inputs: Option<HashMap<String, String>>,
) -> Result<WorkflowRun, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    tokio::task::spawn_blocking(move || {
        let wf = read_one(&name).ok_or_else(|| format!("workflow '{name}' not found"))?;
        build_run(wf, inputs.unwrap_or_default())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_home::with_temp_home;

    #[test]
    fn list_empty_when_dir_missing() {
        with_temp_home(|_| {
            assert!(read_all().is_empty());
        });
    }

    #[test]
    fn seed_then_list() {
        with_temp_home(|_| {
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
        with_temp_home(|_| {
            let wf = Workflow {
                name: "demo".into(),
                description: Some("d".into()),
                inputs: vec![],
                steps: vec![WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "hi".into(),
                    model: None,
                    pipe_output: false,
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
        with_temp_home(|_| {
            assert!(read_one("../etc/passwd").is_none());
            assert!(read_one("sub/dir").is_none());
            assert!(write_one(&Workflow {
                name: "../evil".into(),
                description: None,
                inputs: vec![],
                steps: vec![],
            })
            .is_err());
        });
    }

    #[test]
    fn name_falls_back_to_filename() {
        with_temp_home(|_| {
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

    // ---------- v2 (inputs / model / pipe_output) ----------

    /// A v1 fixture (no v2 fields anywhere) must parse with empty/false v2
    /// defaults and run to byte-identical `WorkflowRun.steps`.
    #[test]
    fn v1_yaml_runs_byte_identically() {
        let raw = "name: legacy\n\
                   description: v1 fixture\n\
                   steps:\n  \
                     - role: code-reviewer\n    \
                       prompt: \"Review the diff. Note {{this}} is literal text in v1.\"\n  \
                     - role: test-writer\n    \
                       prompt: \"Write tests.\"\n";
        let wf = parse_workflow(raw, "legacy").unwrap();
        assert!(wf.inputs.is_empty());
        assert!(wf.steps.iter().all(|s| s.model.is_none() && !s.pipe_output));
        let run = build_run(wf.clone(), HashMap::new()).unwrap();
        // Byte-identical: no expansion ran (would have errored on {{this}}),
        // and the serialized steps carry no v2 keys at all.
        assert_eq!(run.steps, wf.steps);
        let json = serde_json::to_string(&run.steps[1]).unwrap();
        assert_eq!(json, r#"{"role":"test-writer","prompt":"Write tests."}"#);
        // Same guarantee on disk: a v1 workflow re-serializes without v2 keys.
        let yaml = serde_yaml::to_string(&wf).unwrap();
        assert!(!yaml.contains("inputs"));
        assert!(!yaml.contains("model"));
        assert!(!yaml.contains("pipe_output"));
    }

    #[test]
    fn v2_yaml_parses_new_fields() {
        let raw = "name: release-notes\n\
                   inputs:\n  \
                     - key: tag\n    \
                       label: Release tag\n    \
                       required: true\n  \
                     - key: tone\n    \
                       default: neutral\n\
                   steps:\n  \
                     - role: docs-writer\n    \
                       prompt: \"Draft notes since {{tag}} in a {{tone}} tone.\"\n    \
                       model: \"ollama:llama3.1\"\n    \
                       pipe_output: true\n  \
                     - role: code-reviewer\n    \
                       prompt: \"Tighten the draft above.\"\n";
        let wf = parse_workflow(raw, "release-notes").unwrap();
        assert_eq!(wf.inputs.len(), 2);
        assert!(wf.inputs[0].required);
        assert_eq!(wf.inputs[1].default.as_deref(), Some("neutral"));
        assert_eq!(wf.steps[0].model.as_deref(), Some("ollama:llama3.1"));
        assert!(wf.steps[0].pipe_output);
        assert!(!wf.steps[1].pipe_output);
    }

    fn templated_workflow() -> Workflow {
        Workflow {
            name: "release-notes".into(),
            description: None,
            inputs: vec![
                WorkflowInput {
                    key: "tag".into(),
                    label: Some("Release tag".into()),
                    default: None,
                    required: true,
                },
                WorkflowInput {
                    key: "tone".into(),
                    label: None,
                    default: Some("neutral".into()),
                    required: false,
                },
            ],
            steps: vec![
                WorkflowStep {
                    role: "docs-writer".into(),
                    prompt: "Draft notes since {{tag}} in a {{tone}} tone.".into(),
                    model: Some("ollama:llama3.1".into()),
                    pipe_output: true,
                },
                WorkflowStep {
                    role: "code-reviewer".into(),
                    prompt: "Tighten the draft for {{tag}}.".into(),
                    model: None,
                    pipe_output: false,
                },
            ],
        }
    }

    #[test]
    fn expansion_happy_path_uses_provided_and_default() {
        let provided: HashMap<String, String> =
            [("tag".to_string(), "v3.2.0".to_string())].into();
        let run = build_run(templated_workflow(), provided).unwrap();
        assert_eq!(
            run.steps[0].prompt,
            "Draft notes since v3.2.0 in a neutral tone."
        );
        assert_eq!(run.steps[1].prompt, "Tighten the draft for v3.2.0.");
    }

    #[test]
    fn expansion_missing_required_key_errors() {
        let err = build_run(templated_workflow(), HashMap::new()).unwrap_err();
        assert!(err.contains("missing required input 'tag'"), "got: {err}");
        // Blank counts as missing for a required input.
        let blank: HashMap<String, String> = [("tag".to_string(), "  ".to_string())].into();
        let err = build_run(templated_workflow(), blank).unwrap_err();
        assert!(err.contains("missing required input 'tag'"), "got: {err}");
    }

    #[test]
    fn expansion_unknown_placeholder_errors() {
        let mut wf = templated_workflow();
        wf.steps[1].prompt = "Also mention {{codename}}.".into();
        let provided: HashMap<String, String> =
            [("tag".to_string(), "v3.2.0".to_string())].into();
        let err = build_run(wf, provided).unwrap_err();
        assert!(err.contains("unknown input '{{codename}}'"), "got: {err}");
    }

    #[test]
    fn expansion_undeclared_provided_key_errors() {
        let provided: HashMap<String, String> = [
            ("tag".to_string(), "v3.2.0".to_string()),
            ("oops".to_string(), "x".to_string()),
        ]
        .into();
        let err = build_run(templated_workflow(), provided).unwrap_err();
        assert!(err.contains("'oops' is not declared"), "got: {err}");
    }

    #[test]
    fn expansion_is_exact_key_only() {
        let mut wf = templated_workflow();
        // Spaces, uppercase, dots, nesting: none of these are placeholders.
        wf.steps[0].prompt =
            "{{ tag }} {{TAG}} {{a.b}} {{tag}} {{{tag}}}".into();
        let provided: HashMap<String, String> =
            [("tag".to_string(), "v1".to_string())].into();
        let run = build_run(wf, provided).unwrap();
        // `{{{tag}}}` = literal `{` + placeholder + literal `}` — exact-key
        // matching still fires on the inner `{{tag}}`.
        assert_eq!(run.steps[0].prompt, "{{ tag }} {{TAG}} {{a.b}} v1 {v1}");
    }

    #[test]
    fn inputs_rejected_when_none_declared() {
        let mut wf = templated_workflow();
        wf.inputs = vec![];
        wf.steps[0].prompt = "static".into();
        wf.steps[1].prompt = "static too".into();
        let provided: HashMap<String, String> = [("tag".to_string(), "v1".to_string())].into();
        let err = build_run(wf, provided).unwrap_err();
        assert!(err.contains("declares no inputs"), "got: {err}");
    }

    #[test]
    fn invalid_declared_key_errors() {
        let mut wf = templated_workflow();
        wf.inputs[0].key = "Bad Key!".into();
        let provided: HashMap<String, String> =
            [("Bad Key!".to_string(), "v1".to_string())].into();
        let err = build_run(wf, provided).unwrap_err();
        assert!(err.contains("invalid input key"), "got: {err}");
    }

    /// Step order and per-step flags survive expansion verbatim — the
    /// frontend pipes step N's answer into step N+1 based on this ordering.
    #[test]
    fn pipe_output_ordering_preserved() {
        let provided: HashMap<String, String> =
            [("tag".to_string(), "v3.2.0".to_string())].into();
        let run = build_run(templated_workflow(), provided).unwrap();
        assert_eq!(run.steps.len(), 2);
        assert_eq!(run.steps[0].role, "docs-writer");
        assert!(run.steps[0].pipe_output, "step 1 pipes into step 2");
        assert_eq!(run.steps[0].model.as_deref(), Some("ollama:llama3.1"));
        assert_eq!(run.steps[1].role, "code-reviewer");
        assert!(!run.steps[1].pipe_output);
        assert!(run.steps[1].model.is_none());
    }

    // ---------- Full scope: recipe catalog + import/export ----------

    /// The five-recipe catalog seeds alongside the original five v1
    /// defaults — ten total — and every original v1 name survives untouched.
    #[test]
    fn catalog_seed_lands_all_five_recipes() {
        with_temp_home(|_| {
            seed_default_workflows();
            let listed = read_all();
            assert_eq!(listed.len(), 10, "5 v1 defaults + 5 catalog recipes: {:?}",
                listed.iter().map(|w| &w.name).collect::<Vec<_>>());
            let names: Vec<&str> = listed.iter().map(|w| w.name.as_str()).collect();
            for expected in [
                "audit-repo",
                "fix-failing-tests",
                "release-notes",
                "pr-review",
                "security-scan-before-merge",
            ] {
                assert!(names.contains(&expected), "missing catalog recipe '{expected}' in {names:?}");
            }
            for expected in [
                "review-pr",
                "morning-standup",
                "triage-bug",
                "prep-release",
                "audit-deps",
            ] {
                assert!(names.contains(&expected), "missing v1 default '{expected}' in {names:?}");
            }
        });
    }

    /// Every catalog recipe declares at least one input, pins at least one
    /// step's model (the cheap-local-for-summarize / Auto-for-review
    /// policy), and actually resolves end to end when its required inputs
    /// are supplied.
    #[test]
    fn catalog_recipes_are_parameterized_and_runnable() {
        for wf in recipe_catalog() {
            assert!(
                !wf.inputs.is_empty(),
                "'{}' should declare at least one input",
                wf.name
            );
            assert!(
                wf.steps.iter().any(|s| s.model.is_some()),
                "'{}' should pin at least one step's model",
                wf.name
            );
            let provided: HashMap<String, String> = wf
                .inputs
                .iter()
                .filter(|i| i.required)
                .map(|i| (i.key.clone(), "x".to_string()))
                .collect();
            let run = build_run(wf.clone(), provided)
                .unwrap_or_else(|e| panic!("'{}' failed to run: {e}", wf.name));
            assert!(!run.steps.is_empty());
        }
    }

    /// Export → import round-trips a v2 workflow field-for-field, and
    /// re-exporting the imported copy produces byte-identical YAML.
    #[test]
    fn export_import_round_trip_v2() {
        let wf = templated_workflow();
        let yaml = workflow_to_yaml(&wf).unwrap();
        let imported = import_from_text(&yaml, "fallback").unwrap();
        assert_eq!(imported, wf);
        let yaml2 = workflow_to_yaml(&imported).unwrap();
        assert_eq!(yaml, yaml2);
    }

    /// A v1 workflow round-trips through export/import with no v2 keys
    /// introduced anywhere in the YAML — matches the back-compat guarantee
    /// `v1_yaml_runs_byte_identically` verifies for parse/run.
    #[test]
    fn export_import_round_trip_v1() {
        let wf = Workflow {
            name: "legacy-import".into(),
            description: Some("v1 fixture".into()),
            inputs: vec![],
            steps: vec![WorkflowStep {
                role: "code-reviewer".into(),
                prompt: "Review the diff.".into(),
                model: None,
                pipe_output: false,
            }],
        };
        let yaml = workflow_to_yaml(&wf).unwrap();
        assert!(!yaml.contains("inputs"));
        assert!(!yaml.contains("model"));
        assert!(!yaml.contains("pipe_output"));
        let imported = import_from_text(&yaml, "fallback").unwrap();
        assert_eq!(imported, wf);
    }

    #[test]
    fn import_rejects_yaml_with_no_steps() {
        let err = import_from_text("name: empty\nsteps: []\n", "fallback").unwrap_err();
        assert!(err.contains("has no steps"), "got: {err}");
    }

    #[test]
    fn import_rejects_unsafe_name() {
        let err = import_from_text(
            "name: \"../evil\"\nsteps:\n  - role: code-reviewer\n    prompt: hi\n",
            "fallback",
        )
        .unwrap_err();
        assert!(err.contains("invalid workflow name"), "got: {err}");
    }

    #[test]
    fn import_uses_fallback_name_when_yaml_omits_it() {
        let wf = import_from_text(
            "steps:\n  - role: code-reviewer\n    prompt: hi\n",
            "from-filename",
        )
        .unwrap();
        assert_eq!(wf.name, "from-filename");
    }
}
