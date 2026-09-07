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
use crate::observability::tracing_store::TracingStore;
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

// ── Retrieval-quality eval (Brain/RAG baseline) ─────────────────────────────
//
// Measures the RETRIEVAL half of the unified RAG path (issue 010): each task
// embeds a query and checks that the expected sources appear among the top-k
// citations returned by `brain_rag::retrieve_unified`. Deterministic substring
// scoring over the retrieved references (same philosophy as `score_answer`);
// no generation model is involved, so a run is cheap (one local embed call per
// query) and fully offline. Reports persist to a SEPARATE history file so
// before/after comparisons are possible across index or ranking changes
// without touching the model-benchmark history the Evals panel renders.
//
// Expectations are corpus-specific (they name the user's own notes/sessions),
// so there are NO built-in default tasks: the fixture lives at
// `~/.cortex/retrieval-eval-tasks.json` (see
// `src-tauri/fixtures/retrieval-eval-tasks.example.json` for the shape).

/// One retrieval-quality probe: a query plus substrings expected to appear
/// (case-insensitively) among the references/paths of the top-k retrieved
/// citations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvalTask {
    pub id: String,
    pub query: String,
    #[serde(default)]
    pub expect_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvalResult {
    pub id: String,
    pub query: String,
    pub passed: bool,
    /// Fraction of `expect_sources` found among the retrieved references.
    pub score: f32,
    pub matched: Vec<String>,
    pub missed: Vec<String>,
    /// Display references of the retrieved top-k (redacted — this report is
    /// persisted to disk), for the audit trail of what retrieval returned.
    pub retrieved: Vec<String>,
    /// How many retrieved chunks were stale (source changed since indexing).
    pub stale_retrieved: usize,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvalReport {
    pub run_id: String,
    /// Embedding model the index + queries used (retrieval has no chat model).
    pub embed_model: String,
    pub started_unix_ms: i64,
    pub finished_unix_ms: i64,
    pub total: usize,
    pub passed: usize,
    pub score_avg: f32,
    /// Top-k depth each query was scored at.
    pub k: usize,
    pub results: Vec<RetrievalEvalResult>,
}

/// Score retrieval hits against expected sources: each expected substring must
/// appear (case-insensitively) in at least one retrieved reference. Score is
/// the fraction of expectations satisfied; an empty expectation list passes
/// trivially (mirrors `score_answer` — useful for smoke tasks that only check
/// "retrieval returned without erroring").
fn score_retrieval(
    retrieved: &[String],
    expect_sources: &[String],
) -> (bool, f32, Vec<String>, Vec<String>) {
    if expect_sources.is_empty() {
        return (true, 1.0, vec![], vec![]);
    }
    let hay: Vec<String> = retrieved.iter().map(|r| r.to_lowercase()).collect();
    let mut matched = Vec::new();
    let mut missed = Vec::new();
    for needle in expect_sources {
        let n = needle.to_lowercase();
        if hay.iter().any(|h| h.contains(&n)) {
            matched.push(needle.clone());
        } else {
            missed.push(needle.clone());
        }
    }
    let score = matched.len() as f32 / expect_sources.len() as f32;
    (missed.is_empty(), score, matched, missed)
}

fn retrieval_tasks_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("retrieval-eval-tasks.json"))
}

fn retrieval_history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("retrieval-eval-history.json"))
}

/// Parse the retrieval fixture: a JSON array of `RetrievalEvalTask`. Returns
/// `None` unless it yields at least one well-formed task (non-empty id AND
/// query), so a typo'd file reads as "no fixture" instead of silently running
/// an empty benchmark.
fn parse_retrieval_tasks(raw: &str) -> Option<Vec<RetrievalEvalTask>> {
    let tasks: Vec<RetrievalEvalTask> = serde_json::from_str(raw).ok()?;
    let tasks: Vec<RetrievalEvalTask> = tasks
        .into_iter()
        .filter(|t| !t.id.trim().is_empty() && !t.query.trim().is_empty())
        .collect();
    if tasks.is_empty() {
        None
    } else {
        Some(tasks)
    }
}

fn load_retrieval_tasks() -> Option<Vec<RetrievalEvalTask>> {
    retrieval_tasks_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| parse_retrieval_tasks(&raw))
}

fn load_retrieval_history() -> Vec<RetrievalEvalReport> {
    retrieval_history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_retrieval_history(reports: &[RetrievalEvalReport]) -> Result<(), String> {
    let path = retrieval_history_path().ok_or("could not resolve ~/.cortex")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(reports).map_err(|e| e.to_string())?;
    // Atomic write so a crash can't leave a torn history file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write retrieval-eval-history.json: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("commit retrieval-eval-history.json: {e}"))
}

/// The user's retrieval fixture, if present and well-formed.
#[tauri::command]
pub fn list_retrieval_eval_tasks() -> Result<Vec<RetrievalEvalTask>, String> {
    Ok(load_retrieval_tasks().unwrap_or_default())
}

#[tauri::command]
pub fn list_retrieval_eval_reports() -> Result<Vec<RetrievalEvalReport>, String> {
    Ok(load_retrieval_history())
}

/// Run the retrieval-quality baseline: embed each fixture query (local Ollama,
/// same embedder the index uses) and score whether the expected sources appear
/// among the top-k citations. `tasks` overrides the fixture file; `persist`
/// defaults to true (history capped like the model-benchmark history).
#[tauri::command]
pub async fn run_retrieval_eval(
    tasks: Option<Vec<RetrievalEvalTask>>,
    k: Option<usize>,
    persist: Option<bool>,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<RetrievalEvalReport, String> {
    let tasks = match tasks.filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => load_retrieval_tasks().ok_or_else(|| {
            format!(
                "no retrieval eval fixture — create {} (a JSON array of \
                 {{\"id\", \"query\", \"expect_sources\"}}; see \
                 fixtures/retrieval-eval-tasks.example.json in the repo)",
                retrieval_tasks_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/.cortex/retrieval-eval-tasks.json".into())
            )
        })?,
    };
    let k = k.unwrap_or(8).clamp(1, 12);
    let (ollama_base, vault) = {
        let cfg = state.config.read();
        (cfg.ollama_base_url.clone(), cfg.obsidian_vault.clone())
    };
    let embed_model = crate::memory::embed::embed_model();

    let started = now_ms();
    let total = tasks.len();
    let mut results = Vec::with_capacity(total);
    for task in &tasks {
        let t0 = now_ms();
        let cites = crate::commands::brain_rag::retrieve_unified(
            store.inner(),
            &ollama_base,
            vault.clone(),
            None,
            &task.query,
            k,
        )
        .await;
        let latency_ms = (now_ms() - t0).max(0) as u64;
        // Match expectations against BOTH the display reference and the
        // absolute open_path, so a fixture can name a vault-relative note, an
        // absolute path fragment, or a chat session id.
        let haystacks: Vec<String> = cites
            .iter()
            .map(|c| format!("{} {}", c.reference, c.open_path))
            .collect();
        let (passed, score, matched, missed) = score_retrieval(&haystacks, &task.expect_sources);
        results.push(RetrievalEvalResult {
            id: task.id.clone(),
            // The report is persisted to disk → everything free-form passes
            // through the redaction choke-point. Snippets are deliberately NOT
            // stored (indexed content may contain secrets); references are
            // paths/session ids, redacted anyway for defense in depth.
            query: crate::redact::redact_text(&task.query),
            passed,
            score,
            matched,
            missed,
            retrieved: cites.iter().map(|c| crate::redact::redact_text(&c.reference)).collect(),
            stale_retrieved: cites.iter().filter(|c| c.stale).count(),
            latency_ms,
        });
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let score_avg = if total > 0 {
        results.iter().map(|r| r.score).sum::<f32>() / total as f32
    } else {
        0.0
    };
    let report = RetrievalEvalReport {
        run_id: format!("retrieval-{}-{}", started, RUN_SEQ.fetch_add(1, Ordering::Relaxed)),
        embed_model,
        started_unix_ms: started,
        finished_unix_ms: now_ms(),
        total,
        passed,
        score_avg,
        k,
        results,
    };
    if persist.unwrap_or(true) {
        let mut history = load_retrieval_history();
        history.insert(0, report.clone());
        history.truncate(20);
        let _ = save_retrieval_history(&history);
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

    // ── Retrieval-quality baseline (issue 010) ──────────────────────────

    #[test]
    fn retrieval_full_match_passes() {
        let retrieved = ex(&["homelab-topology.md C:\\vault\\homelab-topology.md", "sess-abc123 "]);
        let (passed, score, matched, missed) =
            score_retrieval(&retrieved, &ex(&["homelab", "sess-abc"]));
        assert!(passed);
        assert_eq!(score, 1.0);
        assert_eq!(matched.len(), 2);
        assert!(missed.is_empty());
    }

    #[test]
    fn retrieval_partial_match_fails_with_fractional_score() {
        let retrieved = ex(&["notes/homelab.md "]);
        let (passed, score, _, missed) =
            score_retrieval(&retrieved, &ex(&["homelab", "does-not-exist"]));
        assert!(!passed);
        assert!((score - 0.5).abs() < 0.001);
        assert_eq!(missed, vec!["does-not-exist".to_string()]);
    }

    #[test]
    fn retrieval_matching_is_case_insensitive_and_empty_rubric_passes() {
        let retrieved = ex(&["Notes/HomeLab.md "]);
        let (passed, _, _, _) = score_retrieval(&retrieved, &ex(&["homelab"]));
        assert!(passed);
        // Empty expectations: smoke task, passes trivially (mirrors score_answer).
        let (passed, score, _, _) = score_retrieval(&retrieved, &[]);
        assert!(passed);
        assert_eq!(score, 1.0);
        // But an expectation against EMPTY retrieval must fail.
        let (passed, score, _, _) = score_retrieval(&[], &ex(&["homelab"]));
        assert!(!passed);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn retrieval_tasks_parse_and_filter_malformed_entries() {
        let raw = r#"[
            {"id": "good", "query": "homelab tunnel setup", "expect_sources": ["homelab"]},
            {"id": "", "query": "missing id"},
            {"id": "no-query", "query": "   "}
        ]"#;
        let tasks = parse_retrieval_tasks(raw).expect("one valid task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "good");
        assert!(parse_retrieval_tasks("not json").is_none());
        assert!(parse_retrieval_tasks("[]").is_none());
        assert!(parse_retrieval_tasks(r#"{"id":"x","query":"y"}"#).is_none());
    }

    #[test]
    fn retrieval_example_fixture_is_well_formed() {
        // The checked-in example fixture (the documented baseline shape) must
        // always parse — it's what users copy to ~/.cortex/retrieval-eval-tasks.json.
        let raw = include_str!("../../fixtures/retrieval-eval-tasks.example.json");
        let tasks = parse_retrieval_tasks(raw).expect("example fixture parses");
        assert!(tasks.len() >= 2);
        let mut seen = std::collections::HashSet::new();
        for t in &tasks {
            assert!(!t.query.trim().is_empty());
            assert!(seen.insert(t.id.clone()), "duplicate task id: {}", t.id);
        }
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
