//! Agent eval / benchmark harness.
//!
//! Runs a model against a set of coding-skill tasks and scores each result
//! against a simple substring rubric (deterministic + offline-checkable),
//! emitting a scored report. Each task carries `expect_contains` strings the
//! answer should include; the score is the fraction matched and a task passes
//! when all are present. Reports are appended to `~/.cortex/eval-history.json`
//! so runs are comparable over time — and across models: the run takes an
//! optional model slug (anything the composer picker offers) routed through
//! the adapter registry via `agents::oneshot`, so the Ollama model the
//! Cookbook just pulled, a Claude CLI model, and the gateway default are all
//! benchmarkable side by side.
//!
//! Task set: the built-ins are coding-task rubrics (code reading, bug
//! spotting, complexity, SQL/git/JS/Rust fundamentals). A user-supplied
//! `~/.cortex/eval-tasks.json` (JSON array of `{id, prompt, expect_contains}`)
//! replaces them when present and well-formed.
//!
//! Why substring-rubric rather than LLM-as-judge: it makes the harness's own
//! scoring deterministic and unit-testable without a second model call, while
//! still exercising the real model end-to-end per task.

use crate::agents::oneshot;
use crate::app_state::AppState;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{Emitter, State};

/// Monotonic suffix so two runs that start in the same millisecond still get
/// distinct run ids (used as React keys + history keys).
static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTask {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub expect_contains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub id: String,
    pub prompt: String,
    pub answer: String,
    pub passed: bool,
    pub score: f32,
    pub matched: Vec<String>,
    pub missed: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub run_id: String,
    pub model: String,
    pub started_unix_ms: i64,
    pub finished_unix_ms: i64,
    pub total: usize,
    pub passed: usize,
    pub score_avg: f32,
    pub results: Vec<EvalResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalProgress {
    done: usize,
    total: usize,
    id: String,
    passed: bool,
    /// Display model for this run (the requested slug, or the gateway default
    /// when none was picked) — lets the jobs pill / panel say what's being
    /// benchmarked.
    model: String,
}

/// The eval run currently in flight (latest progress), if any. Mirrors
/// `cookbook::ACTIVE_PULLS` / `deep_research::ACTIVE_RESEARCH`: the run
/// outlives the `invoke()` that started it, so after a webview reload the
/// frontend job store queries `eval_active` to re-adopt it. `Option` because
/// progress streams over ONE shared `eval:progress` event — `run_eval`
/// rejects a concurrent second run.
static ACTIVE_EVAL: Lazy<Mutex<Option<EvalProgress>>> = Lazy::new(|| Mutex::new(None));

// Built-in coding-task rubrics: code reading, bug spotting, complexity, and
// language/tooling fundamentals a coding agent must not fumble. Deterministic
// substring checks keep scoring offline-verifiable; needles are chosen to be
// unlikely to appear incidentally in a wrong answer.
fn default_tasks() -> Vec<EvalTask> {
    let t = |id: &str, prompt: &str, expect: &[&str]| EvalTask {
        id: id.to_string(),
        prompt: prompt.to_string(),
        expect_contains: expect.iter().map(|s| s.to_string()).collect(),
    };
    vec![
        t(
            "code-reading",
            "Here is a Python function:\n\ndef f(n):\n    total = 0\n    for i in range(n):\n        total += i * i\n    return total\n\nWhat does f(3) return? Answer with just the number.",
            &["5"],
        ),
        t(
            "bug-spotting",
            "This Python function is meant to return the last element of a non-empty list, but it raises an exception:\n\ndef last(xs):\n    return xs[len(xs)]\n\nWhich built-in exception does it raise? Answer with just the exception name.",
            &["indexerror"],
        ),
        t(
            "complexity",
            "What is the worst-case time complexity of binary search on a sorted array of n elements? Answer in big-O notation.",
            &["log"],
        ),
        t(
            "sql-join",
            "In SQL, which type of JOIN returns only the rows that have matching values in both tables? Answer with just the join type.",
            &["inner"],
        ),
        t(
            "js-equality",
            "In JavaScript, which comparison operator tests equality without performing type coercion? Reply with just the operator.",
            &["==="],
        ),
        t(
            "rust-mutability",
            "In Rust, which keyword marks a variable binding as mutable? Answer with just the keyword.",
            &["mut"],
        ),
        t(
            "http-semantics",
            "Which HTTP status code indicates that the requested resource was not found? Answer with just the number.",
            &["404"],
        ),
        t(
            "git-workflow",
            "Which modern git subcommand (introduced in Git 2.23 to take over branch switching from checkout) switches branches? Answer with just the subcommand.",
            &["switch"],
        ),
    ]
}

// ----- custom task file -----

fn custom_tasks_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("eval-tasks.json"))
}

/// Parse a user-supplied task file: a JSON array of `EvalTask`. Returns `None`
/// unless it yields at least one well-formed task (non-empty id AND prompt),
/// so a typo'd file falls back to the built-ins instead of silently running
/// an empty or broken benchmark.
fn parse_custom_tasks(raw: &str) -> Option<Vec<EvalTask>> {
    let tasks: Vec<EvalTask> = serde_json::from_str(raw).ok()?;
    let tasks: Vec<EvalTask> = tasks
        .into_iter()
        .filter(|t| !t.id.trim().is_empty() && !t.prompt.trim().is_empty())
        .collect();
    if tasks.is_empty() {
        None
    } else {
        Some(tasks)
    }
}

/// The task set a run actually uses: `~/.cortex/eval-tasks.json` when present
/// and well-formed, else the built-in coding rubrics.
fn effective_tasks() -> Vec<EvalTask> {
    custom_tasks_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| parse_custom_tasks(&raw))
        .unwrap_or_else(default_tasks)
}

/// E2E-only deterministic stand-in for the LLM call (same pattern as
/// `routines::e2e_fake_result`). Under `CORTEX_E2E=1`, `[[e2e:echo]]rest`
/// answers with `rest` verbatim and `[[e2e:err]]` fails — so the probe can
/// drive a full run (progress → report → job store → notification) offline
/// with both verdicts and an arbitrary model string, without dialing any
/// backend. Returns `None` for every real prompt; production builds never get
/// here because the env gate is checked first.
fn e2e_fake_result(prompt: &str) -> Option<Result<String, String>> {
    let p = prompt.trim_start();
    if let Some(rest) = p.strip_prefix("[[e2e:echo]]") {
        return Some(Ok(rest.trim().to_string()));
    }
    if p.starts_with("[[e2e:err]]") {
        return Some(Err("e2e fake eval failure".into()));
    }
    None
}

// ----- pure scoring (unit-tested) -----

/// Score an answer against the rubric: case-insensitive substring presence.
/// Returns (passed, score, matched, missed). An empty rubric passes trivially.
fn score_answer(answer: &str, expect_contains: &[String]) -> (bool, f32, Vec<String>, Vec<String>) {
    if expect_contains.is_empty() {
        return (true, 1.0, vec![], vec![]);
    }
    let hay = answer.to_lowercase();
    let mut matched = Vec::new();
    let mut missed = Vec::new();
    for needle in expect_contains {
        if hay.contains(&needle.to_lowercase()) {
            matched.push(needle.clone());
        } else {
            missed.push(needle.clone());
        }
    }
    let score = matched.len() as f32 / expect_contains.len() as f32;
    (missed.is_empty(), score, matched, missed)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ----- store -----

fn history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("eval-history.json"))
}

fn load_history() -> Vec<EvalReport> {
    history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_history(reports: &[EvalReport]) -> Result<(), String> {
    let path = history_path().ok_or("could not resolve ~/.cortex")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(reports).map_err(|e| e.to_string())?;
    // Atomic write so a crash/concurrent run can't leave a torn history file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write eval-history.json: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("commit eval-history.json: {e}"))
}

// ----- Tauri commands -----

#[tauri::command]
pub fn list_eval_tasks() -> Result<Vec<EvalTask>, String> {
    Ok(effective_tasks())
}

#[tauri::command]
pub fn list_eval_reports() -> Result<Vec<EvalReport>, String> {
    Ok(load_history())
}

/// Snapshot of the eval run currently in flight, if any. The frontend job
/// store queries this on boot so a webview reload mid-run re-adopts the
/// running job instead of orphaning it.
#[tauri::command]
pub fn eval_active() -> Result<Option<EvalProgress>, String> {
    Ok(ACTIVE_EVAL.lock().clone())
}

/// Run the benchmark: each task is sent to the model and scored against its
/// rubric. Progress streams over `eval:progress`. `model` is any slug the
/// composer's picker offers (`claude-…`, `gpt-…`, `ollama:tag`); `None` keeps
/// the default route (the configured gateway model). The report is persisted
/// to history unless `persist` is explicitly false (the E2E probe drives a
/// real run through this command and must not pollute the user's run history).
#[tauri::command]
pub async fn run_eval(
    tasks: Option<Vec<EvalTask>>,
    model: Option<String>,
    persist: Option<bool>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<EvalReport, String> {
    let tasks = tasks.filter(|t| !t.is_empty()).unwrap_or_else(effective_tasks);
    let model = model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
    // What the report/history rows display: the requested slug, else the
    // configured gateway model (the default route's upstream).
    let display_model = match &model {
        Some(m) => m.clone(),
        None => state.config.read().gateway_model.clone(),
    };
    {
        let mut active = ACTIVE_EVAL.lock();
        if active.is_some() {
            return Err("An eval run is already in progress.".into());
        }
        *active = Some(EvalProgress {
            done: 0,
            total: tasks.len(),
            id: String::new(),
            passed: false,
            model: display_model.clone(),
        });
    }
    let result = run_eval_inner(tasks, model, display_model, persist.unwrap_or(true), app, state).await;
    *ACTIVE_EVAL.lock() = None;
    result
}

async fn run_eval_inner(
    tasks: Vec<EvalTask>,
    model: Option<String>,
    display_model: String,
    persist: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<EvalReport, String> {
    // Build the model fallback chain ONCE (primary + any configured
    // fallbacks). Each task routes through the SAME chat-path routing per call
    // and self-heals transient provider blips (retry+backoff, then fallback).
    let chain = oneshot::fallback_chain(model.clone());
    let policy = oneshot::RetryPolicy::default();

    let started = now_ms();
    let total = tasks.len();
    let mut results = Vec::with_capacity(total);

    for (i, task) in tasks.iter().enumerate() {
        let t0 = now_ms();
        let outcome = match crate::commands::e2e::e2e_enabled()
            .then(|| e2e_fake_result(&task.prompt))
            .flatten()
        {
            Some(fake) => fake,
            None => {
                oneshot::complete_with_fallback(&state.registry, &chain, &task.prompt, &policy)
                    .await
                    .map(|o| o.text)
            }
        };
        let latency_ms = (now_ms() - t0).max(0) as u64;
        let result = match outcome {
            Ok(answer) => {
                let (passed, score, matched, missed) = score_answer(&answer, &task.expect_contains);
                EvalResult {
                    id: task.id.clone(),
                    prompt: task.prompt.clone(),
                    answer,
                    passed,
                    score,
                    matched,
                    missed,
                    latency_ms,
                    error: None,
                }
            }
            Err(e) => EvalResult {
                id: task.id.clone(),
                prompt: task.prompt.clone(),
                answer: String::new(),
                passed: false,
                score: 0.0,
                matched: vec![],
                missed: task.expect_contains.clone(),
                latency_ms,
                error: Some(e),
            },
        };
        let progress = EvalProgress {
            done: i + 1,
            total,
            id: result.id.clone(),
            passed: result.passed,
            model: display_model.clone(),
        };
        // Keep the in-flight registry current so a reload re-adopts the run at
        // its real progress, not 0/N.
        if let Some(slot) = ACTIVE_EVAL.lock().as_mut() {
            *slot = progress.clone();
        }
        let _ = app.emit("eval:progress", progress);
        results.push(result);
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let score_avg = if total > 0 {
        results.iter().map(|r| r.score).sum::<f32>() / total as f32
    } else {
        0.0
    };
    let report = EvalReport {
        run_id: format!("eval-{}-{}", started, RUN_SEQ.fetch_add(1, Ordering::Relaxed)),
        model: display_model,
        started_unix_ms: started,
        finished_unix_ms: now_ms(),
        total,
        passed,
        score_avg,
        results,
    };

    // append to history (most-recent first, capped)
    if persist {
        let mut history = load_history();
        history.insert(0, report.clone());
        history.truncate(20);
        let _ = save_history(&history);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn full_match_passes_with_score_one() {
        let (passed, score, matched, missed) =
            score_answer("The answer is 4.", &ex(&["4"]));
        assert!(passed);
        assert_eq!(score, 1.0);
        assert_eq!(matched, vec!["4".to_string()]);
        assert!(missed.is_empty());
    }

    #[test]
    fn case_insensitive_and_multi_term() {
        let (passed, score, _, missed) = score_answer(
            "HyperText Transfer Protocol",
            &ex(&["hypertext", "transfer", "protocol"]),
        );
        assert!(passed);
        assert_eq!(score, 1.0);
        assert!(missed.is_empty());
    }

    #[test]
    fn partial_match_fails_with_fractional_score() {
        let (passed, score, matched, missed) =
            score_answer("HyperText Protocol", &ex(&["hypertext", "transfer", "protocol"]));
        assert!(!passed);
        assert!((score - 2.0 / 3.0).abs() < 0.001);
        assert_eq!(matched.len(), 2);
        assert_eq!(missed, vec!["transfer".to_string()]);
    }

    #[test]
    fn empty_rubric_passes_trivially() {
        let (passed, score, _, _) = score_answer("anything", &[]);
        assert!(passed);
        assert_eq!(score, 1.0);
    }

    #[test]
    fn default_tasks_are_well_formed() {
        let tasks = default_tasks();
        assert!(tasks.len() >= 5);
        assert!(tasks.iter().all(|t| !t.id.is_empty() && !t.prompt.is_empty() && !t.expect_contains.is_empty()));
        // Unique ids: they're React keys + per-task history keys.
        let mut seen = std::collections::HashSet::new();
        for t in &tasks {
            assert!(seen.insert(t.id.clone()), "duplicate task id: {}", t.id);
        }
    }

    #[test]
    fn custom_tasks_parse_and_filter_malformed_entries() {
        let raw = r#"[
            {"id": "mine", "prompt": "What is ownership in Rust?", "expect_contains": ["borrow"]},
            {"id": "", "prompt": "missing id"},
            {"id": "no-prompt", "prompt": "   "}
        ]"#;
        let tasks = parse_custom_tasks(raw).expect("one valid task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "mine");
    }

    #[test]
    fn custom_tasks_reject_garbage_and_empty() {
        assert!(parse_custom_tasks("not json").is_none());
        assert!(parse_custom_tasks("[]").is_none());
        assert!(parse_custom_tasks(r#"[{"id":"","prompt":""}]"#).is_none());
        // A non-array (e.g. an object) must not parse.
        assert!(parse_custom_tasks(r#"{"id":"x","prompt":"y"}"#).is_none());
    }

    #[test]
    fn e2e_markers_parse_both_outcomes_and_ignore_real_prompts() {
        assert_eq!(
            e2e_fake_result("[[e2e:echo]] pong"),
            Some(Ok("pong".to_string()))
        );
        assert!(matches!(e2e_fake_result("[[e2e:err]] anything"), Some(Err(_))));
        assert_eq!(e2e_fake_result("What is 2 + 2?"), None);
    }
}
