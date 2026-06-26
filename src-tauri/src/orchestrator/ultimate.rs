//! The "ultimate" multi-model agent — a Fable-5-like HYBRID orchestrator.
//!
//! Where [`team_run`](crate::orchestrator::team_run) fans a static roster of
//! workers across ONE model each, this engine treats the *model set itself* as
//! the lever: a lead model decomposes a goal into subtasks, each subtask is
//! routed to the best model for its difficulty, and the high-value / uncertain
//! ones are FANNED OUT across several DISTINCT models in parallel. A critical
//! aggregator then MERGES the competing candidates (not a vote — it resolves
//! conflicts and keeps the strongest of each), and a final synthesis pass folds
//! every subtask result into one coherent deliverable + a verification note.
//!
//! Shape mirrors `team_run` deliberately: Tauri-free (registry + tracing store
//! in, an `emit` callback out), so the whole pipeline is exercisable from tests
//! with stub adapters. The streaming [`UltEvent`]s are serde-serializable so the
//! command wrapper (added later) can forward them to the UI verbatim.
//!
//! ## What is and isn't wired
//!
//! - Chat subtasks are fully wired: each model in a subtask's set runs a real
//!   [`oneshot::complete_resilient`] completion in parallel.
//! - CODE subtasks with a valid git `project_root` are now wired to the
//!   **git-worktree** path: each chosen code-capable model runs IN ITS OWN
//!   worktree (a real CLI adapter, cwd = the worktree, so it edits files), the
//!   per-candidate `git diff` is captured, then — when ≥2 models produced a
//!   non-empty diff — a strong model AI-AUTO-MERGES the candidate diffs into one
//!   unified diff that is APPLIED to a fresh merge worktree and VERIFIED (build/
//!   test detection). If that merge applies cleanly and verification passes (or
//!   there is no harness) the merge worktree/branch IS the subtask result; if it
//!   fails to apply or fails verification the engine FALLS BACK to the older
//!   SELECT-the-best-candidate behavior (keep one winner's branch). A best-effort
//!   test command verifies the surviving branch, every other worktree is torn
//!   down, and the surviving branch is kept for the user to review (see
//!   [`run_code_subtask_worktrees`]). CODE subtasks WITHOUT a code-capable model
//!   (or without a git project root) transparently fall back to the chat path.

use crate::agents::adapter::{AgentCapability, AgentEvent, ChatRequest, ChatTurn};
use crate::agents::{oneshot, Registry};
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use crate::orchestrator::cost_router::{self, Difficulty};
use crate::orchestrator::team_run::{TaskDifficulty, TaskKind};
use crate::pricing::compute_usd;
use crate::worktrees::{self, Worktree, WorktreeStore};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Wall-clock budget for one model's coding run inside its worktree — matches
/// `team_run::WORKER_TIMEOUT` so a wedged CLI can't pin a candidate forever.
const CODE_WORKER_TIMEOUT: Duration = Duration::from_secs(420);

/// Wall-clock budget for the best-effort verification command in the winning
/// worktree. Kept short so a slow/hanging suite can't stall the whole run.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(300);

/// Configuration for one ultimate run.
#[derive(Debug, Clone)]
pub struct UltimateConfig {
    /// The high-level goal to decompose and accomplish.
    pub goal: String,
    /// Repo root for code subtasks (a worktree base). `None` → everything runs
    /// on the chat path.
    pub project_root: Option<String>,
    /// How many DISTINCT models a fanned-out subtask is raced across. Defaults
    /// to 3 via [`UltimateConfig::default`]; clamped to ≥1 at use.
    pub fan_out: usize,
    /// Pin the lead/decomposer model. `None` → the strongest capable model the
    /// registry can reach (a `Hard` chat pick).
    pub lead_model: Option<String>,
}

impl Default for UltimateConfig {
    fn default() -> Self {
        Self { goal: String::new(), project_root: None, fan_out: 3, lead_model: None }
    }
}

/// One subtask the lead carved the goal into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSubtask {
    pub id: String,
    pub task: String,
    /// `chat` | `code` — drives capability routing.
    pub kind: String,
    /// `easy` | `hard` — drives cheap-vs-strong routing.
    pub difficulty: String,
    /// When `true` this subtask is high-value/uncertain → race it across
    /// multiple distinct models and merge the candidates.
    pub fan_out: bool,
}

/// Streamed progress event. Serialized to the UI by the command wrapper.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UltEvent {
    /// The lead finished decomposing the goal.
    Plan { subtasks: Vec<PlannedSubtask> },
    /// A subtask began executing across `models`.
    SubtaskStarted { id: String, task: String, models: Vec<String> },
    /// One model finished its candidate for a subtask.
    ModelDone { subtask_id: String, model: String, ok: bool, output: String },
    /// A fanned-out subtask's candidates were merged into one result.
    SubtaskMerged { id: String, merged: String },
    /// The final synthesis pass produced the deliverable.
    Synthesis { merged: String },
    /// Running projected cost (USD) of the whole run.
    Cost { usd: f64 },
    /// The run finished (`ok` = every subtask produced at least one candidate).
    Done { ok: bool },
    /// A fatal error aborted the run.
    Error { msg: String },
}

/// What one subtask produced after execution + (for fan-out) aggregation.
#[derive(Debug, Clone, Serialize)]
pub struct SubtaskResult {
    pub id: String,
    pub task: String,
    /// Distinct model slugs that ran this subtask.
    pub models: Vec<String>,
    /// The merged (fan-out) or single (routed) output.
    pub output: String,
    /// `true` when at least one model produced a candidate.
    pub ok: bool,
}

/// The full result of an ultimate run.
#[derive(Debug, Clone, Serialize)]
pub struct UltimateResult {
    /// The synthesized, coherent deliverable (or the single subtask's output
    /// when there was only one).
    pub final_output: String,
    pub subtasks: Vec<SubtaskResult>,
    pub total_usd: f64,
}

/// The roster of DISTINCT available model slugs we can fan out across.
///
/// Combines (a) every live local `ollama:<tag>`, and (b) every catalog model the
/// registry can currently reach for a `Chat` task (via the cost router's
/// candidate enumeration — `claude-opus-4-8`, `gpt-…`, etc.). De-duplicated,
/// order-stable, always ≥1 when anything is registered (callers should still
/// handle the empty case from a bare registry).
pub async fn discover_models(registry: &RwLock<Registry>) -> Vec<String> {
    let local: Vec<String> =
        crate::agents::ollama::fetch_tags_at(crate::agents::ollama::LOCAL_OLLAMA)
            .await
            .into_iter()
            .map(|t| format!("ollama:{t}"))
            .collect();

    let mut out: Vec<String> = Vec::new();
    let push = |slug: String, out: &mut Vec<String>| {
        let s = slug.trim().to_string();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };

    // Catalog/CLI/API models reachable for a chat task. `candidates` already
    // filters to available + capable adapters and inherits the local tags too,
    // so this single call yields the whole roster.
    let cands =
        cost_router::candidates(&[AgentCapability::Chat], &registry.read(), &local);
    for c in cands {
        push(c.model, &mut out);
    }
    // Belt-and-suspenders: make sure every local tag is present even if the
    // Ollama adapter wasn't registered as a candidate source.
    for l in local {
        push(l, &mut out);
    }
    out
}

/// Build the lead's decomposition prompt. Asks for strict JSON, one entry per
/// subtask, tagging high-value/uncertain work `fan_out: true`.
fn build_plan_prompt(goal: &str, roster: &[String]) -> String {
    let models = roster.join(", ");
    format!(
        "You are the lead orchestrator. Decompose the goal below into a small set \
         of concrete, self-contained subtasks (3–6), each directly actionable from \
         its text alone.\n\n\
         Goal: {goal}\n\n\
         Available models you may route to: {models}\n\n\
         For each subtask choose a \"kind\" (\"code\" if it edits/runs a repository, \
         else \"chat\"), a \"difficulty\" (\"easy\" for routine work, \"hard\" for \
         demanding work), and set \"fan_out\": true ONLY for high-value, uncertain, \
         or architecturally-significant subtasks that benefit from racing several \
         models and merging the best — otherwise false.\n\
         Respond with ONLY a JSON array — no prose, no code fences:\n\
         [{{\"id\": \"s1\", \"task\": \"<the subtask>\", \"kind\": \"chat|code\", \"difficulty\": \"easy|hard\", \"fan_out\": true|false}}]"
    )
}

/// Lenient deserialization target for the lead's plan — tags are free strings so
/// one bad value can't fail the whole-array parse.
#[derive(Debug, Deserialize)]
struct RawSubtask {
    #[serde(default)]
    id: Option<String>,
    task: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(default)]
    fan_out: Option<bool>,
}

/// First balanced-bracket `[…]` span in the text, fences and prose ignored.
/// (Copied from `team_run` — the same tolerant extraction small local models
/// need; kept private here to keep the engine self-contained.)
fn extract_json_array(raw: &str) -> Option<String> {
    let start = raw.find('[')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    for (i, c) in raw[start..].char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match c {
            '\\' if in_str => escape = true,
            '"' => in_str = !in_str,
            '[' if !in_str => depth += 1,
            ']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the lead's plan into [`PlannedSubtask`]s. Tolerant by design: extract
/// the first `[…]` span, keep entries with a non-empty task, normalize tags with
/// defaults, and synthesize a stable `id` (`s1`, `s2`, …) when one is missing.
/// A completely unparseable plan degrades to a single chat subtask on the
/// verbatim goal so a sloppy lead never strands the run.
fn parse_plan(raw: &str, goal: &str) -> Vec<PlannedSubtask> {
    let parsed: Vec<RawSubtask> = extract_json_array(raw)
        .and_then(|span| serde_json::from_str::<Vec<RawSubtask>>(&span).ok())
        .unwrap_or_default();

    let mut out: Vec<PlannedSubtask> = Vec::new();
    for (i, r) in parsed.into_iter().enumerate() {
        let task = r.task.trim().to_string();
        if task.is_empty() {
            continue;
        }
        let id = r
            .id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("s{}", i + 1));
        let kind = r
            .kind
            .as_deref()
            .and_then(parse_kind)
            .unwrap_or_default();
        let difficulty = r
            .difficulty
            .as_deref()
            .and_then(parse_difficulty)
            .unwrap_or_default();
        out.push(PlannedSubtask {
            id,
            task,
            kind: kind.as_str().to_string(),
            difficulty: difficulty.as_str().to_string(),
            fan_out: r.fan_out.unwrap_or(false),
        });
    }

    if out.is_empty() {
        out.push(PlannedSubtask {
            id: "s1".into(),
            task: goal.trim().to_string(),
            kind: TaskKind::default().as_str().to_string(),
            difficulty: TaskDifficulty::default().as_str().to_string(),
            fan_out: false,
        });
    }
    out
}

/// Tolerant tag parses, mirroring `team_run`'s synonyms (its enum parsers are
/// private, so we re-derive the small mapping we need here).
fn parse_kind(s: &str) -> Option<TaskKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "chat" | "text" | "write" | "writing" | "review" | "analysis" => Some(TaskKind::Chat),
        "code" | "coding" | "edit" | "repo" | "dev" => Some(TaskKind::Code),
        _ => None,
    }
}

fn parse_difficulty(s: &str) -> Option<TaskDifficulty> {
    match s.trim().to_ascii_lowercase().as_str() {
        "easy" | "trivial" | "simple" | "low" => Some(TaskDifficulty::Easy),
        "medium" | "moderate" | "normal" | "mid" => Some(TaskDifficulty::Medium),
        "hard" | "difficult" | "complex" | "high" => Some(TaskDifficulty::Hard),
        _ => None,
    }
}

fn task_kind(s: &PlannedSubtask) -> TaskKind {
    parse_kind(&s.kind).unwrap_or_default()
}

fn task_difficulty(s: &PlannedSubtask) -> TaskDifficulty {
    parse_difficulty(&s.difficulty).unwrap_or_default()
}

/// Capabilities a subtask of `kind` requires (mirrors `team_run::required_caps`,
/// which is private).
fn required_caps(kind: TaskKind) -> Vec<AgentCapability> {
    match kind {
        TaskKind::Chat => vec![AgentCapability::Chat],
        TaskKind::Code => vec![AgentCapability::Chat, AgentCapability::CodeEdit],
    }
}

/// Map plan difficulty onto the cost router's cheap-vs-strong axis. `Medium`
/// has no clear signal → treated as `Hard` here (the routed single-model path
/// should err toward a capable model rather than the cheapest).
fn cost_difficulty(d: TaskDifficulty) -> Difficulty {
    match d {
        TaskDifficulty::Easy => Difficulty::Easy,
        TaskDifficulty::Medium | TaskDifficulty::Hard => Difficulty::Hard,
    }
}

/// Rough token estimate for cost projection (~4 chars/token), same heuristic as
/// `team_run` (the adapters surface no real per-token usage).
fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Price (`input_per_million`, `output_per_million`) for a concrete slug. Local
/// Ollama slugs are free; everything else priced through the shared table after
/// alias resolution (so `opus` ≈ `claude-opus-4-8`).
fn price_for_slug(slug: &str) -> (f64, f64) {
    let s = slug.trim();
    if s.is_empty() || s.starts_with("ollama:") || s.starts_with("ollama/") {
        return (0.0, 0.0);
    }
    let resolved = crate::orchestrator::aliases::resolve_model(s);
    crate::pricing::lookup_price(&resolved)
}

/// Choose the set of DISTINCT models a subtask runs on.
///
/// - Non-fan-out → exactly one model, routed by difficulty via
///   [`cost_router::pick_model_for`] (falls back to the roster's first when the
///   router finds nothing — e.g. a code task with no code-capable model).
/// - Fan-out → up to `fan_out` distinct models drawn for diversity: the
///   strongest capable, the cheapest capable, then fill from the roster. This is
///   what makes the merge meaningful — different brains, not the same one twice.
fn select_models(
    registry: &RwLock<Registry>,
    subtask: &PlannedSubtask,
    roster: &[String],
    local_tags: &[String],
    fan_out: usize,
) -> Vec<String> {
    let kind = task_kind(subtask);
    let caps = required_caps(kind);
    let diff = cost_difficulty(task_difficulty(subtask));
    let reg = registry.read();

    if !subtask.fan_out {
        let single = cost_router::pick_model_for(diff, &caps, &reg, local_tags)
            .map(|p| p.model)
            .or_else(|| roster.first().cloned());
        return single.into_iter().collect();
    }

    let mut chosen: Vec<String> = Vec::new();
    let push = |m: Option<String>, chosen: &mut Vec<String>| {
        if let Some(m) = m {
            if !chosen.contains(&m) {
                chosen.push(m);
            }
        }
    };
    // Diverse seeds: strongest then cheapest.
    push(
        cost_router::pick_model_for(Difficulty::Hard, &caps, &reg, local_tags).map(|p| p.model),
        &mut chosen,
    );
    push(
        cost_router::pick_model_for(Difficulty::Easy, &caps, &reg, local_tags).map(|p| p.model),
        &mut chosen,
    );
    // Fill from the full roster for additional distinct brains.
    for m in roster {
        if chosen.len() >= fan_out.max(1) {
            break;
        }
        push(Some(m.clone()), &mut chosen);
    }
    chosen.truncate(fan_out.max(1));
    if chosen.is_empty() {
        chosen.extend(roster.first().cloned());
    }
    chosen
}

/// Build a worker prompt for a single subtask + model.
fn build_worker_prompt(goal: &str, task: &str) -> String {
    format!(
        "Overall goal: {goal}\n\n\
         Your assigned subtask: {task}\n\n\
         Complete the subtask now and reply with your finished work product. Be \
         concrete and complete. If the subtask genuinely cannot be done from this \
         prompt alone, say exactly what is missing."
    )
}

/// Build the coding prompt handed to a model running inside its own worktree.
/// Unlike [`build_worker_prompt`] (which asks for the change *as text*), this
/// instructs the model to EDIT THE FILES in its cwd directly — the CLI adapters
/// run with `current_dir = req.project_root`, so "its cwd" is the worktree.
fn build_code_prompt(goal: &str, task: &str) -> String {
    format!(
        "Overall goal: {goal}\n\n\
         Your assigned coding subtask: {task}\n\n\
         You are working inside a git worktree (your current directory). Make the \
         change by EDITING THE FILES directly in this working directory — do not \
         describe the change, perform it. Keep the change focused on the subtask, \
         build/compile-clean if you can, and when finished give a one-paragraph \
         summary of what you changed and why."
    )
}

/// `true` when `root` is a real on-disk git repository (has a `.git` entry).
/// Mirrors the guard `worktrees::create_worktree` enforces, so we can decide to
/// take the worktree path BEFORE attempting to spin one up.
fn is_git_repo(root: &Path) -> bool {
    root.is_dir() && root.join(".git").exists()
}

/// One model's attempt at a code subtask inside a dedicated worktree.
struct CodeCandidate {
    /// The model slug that ran.
    model: String,
    /// The worktree id (for store-backed `remove_worktree`).
    worktree_id: String,
    /// The `cortex/<id>` branch holding this candidate's work.
    branch: String,
    /// The worktree's on-disk path (its cwd while running).
    path: PathBuf,
    /// `git diff --cached` after staging everything — empty when the model made
    /// no on-disk change.
    diff: String,
    /// The model's own prose summary of what it did (the streamed text).
    summary: String,
    /// `true` when the run produced a non-empty diff.
    ok: bool,
    /// Projected USD for this candidate's completion.
    usd: f64,
}

/// Drive one adapter to completion inside `worktree_path`, collecting its
/// streamed text. Mirrors `oneshot::collect_completion` (run + drain the mpsc in
/// lockstep) but builds a [`ChatRequest`] whose `project_root` IS the worktree —
/// THE mechanism by which a CLI adapter edits files in the worktree rather than
/// the live repo. Bounded by [`CODE_WORKER_TIMEOUT`].
async fn run_adapter_in_worktree(
    adapter: Arc<dyn crate::agents::adapter::AgentAdapter>,
    model: &str,
    worktree_path: &Path,
    prompt: &str,
) -> Result<String, String> {
    let req = ChatRequest {
        session_id: format!("ultimate-code-{}", uuid::Uuid::new_v4()),
        message: prompt.to_string(),
        project_root: Some(worktree_path.to_path_buf()),
        history: Vec::<ChatTurn>::new(),
        model: Some(model.to_string()),
        reasoning_effort: None,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let run = adapter.run(req, tx);
    let collect = async {
        let mut buf = String::new();
        let mut err: Option<String> = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Token { delta } => buf.push_str(&delta),
                AgentEvent::Error { message } => err = Some(message),
                AgentEvent::Done { .. } => break,
                _ => {}
            }
        }
        (buf, err)
    };
    let driven = async {
        let (run_res, (buf, err)) = tokio::join!(run, collect);
        // Unlike the chat path we do NOT require text — a CLI may edit files and
        // say little. The diff is the real signal; surface an error only when
        // the run itself failed AND nothing was produced.
        if let Err(e) = run_res {
            if buf.trim().is_empty() {
                return Err(format!("agent run failed: {e}"));
            }
        }
        if buf.trim().is_empty() {
            if let Some(e) = err {
                return Err(e);
            }
        }
        Ok(buf)
    };
    tokio::time::timeout(CODE_WORKER_TIMEOUT, driven)
        .await
        .map_err(|_| format!("timed out after {}s", CODE_WORKER_TIMEOUT.as_secs()))?
}

/// Stage everything in `worktree_path` and capture the resulting cached diff.
/// `git add -A` then `git diff --cached` so NEW files (added) and MODIFIED files
/// are both reflected. A non-fatal git hiccup yields an empty diff rather than
/// failing the candidate (the model's prose still stands).
fn capture_worktree_diff(worktree_path: &Path) -> String {
    let _ = crate::sys::no_window("git")
        .arg("-C")
        .arg(worktree_path)
        .arg("add")
        .arg("-A")
        .output();
    let out = crate::sys::no_window("git")
        .arg("-C")
        .arg(worktree_path)
        .arg("diff")
        .arg("--cached")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

/// A short, human-readable diffstat line for an event payload (files + ±lines),
/// derived from a unified diff without shelling out again. Cheap and offline so
/// it's safe to compute in the deterministic path / tests.
fn diff_stat(diff: &str) -> String {
    let files = diff.lines().filter(|l| l.starts_with("diff --git ")).count();
    let added = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    format!("{files} file(s), +{added}/-{removed}")
}

/// Detect a test command for the winning worktree by inspecting its manifest
/// files, returning `(program, args)`. Best-effort and conservative: only the
/// three common ecosystems, and only when an unambiguous signal is present.
/// `None` → "no test harness detected" (verification is skipped, not failed).
fn detect_test_command(worktree_path: &Path) -> Option<(String, Vec<String>)> {
    // Rust: a Cargo.toml → `cargo test`.
    if worktree_path.join("Cargo.toml").exists() {
        return Some(("cargo".into(), vec!["test".into()]));
    }
    // JS/TS: a package.json that declares a "test" script → the package manager
    // (prefer pnpm when a pnpm-lock is present, else npm).
    let pkg = worktree_path.join("package.json");
    if pkg.exists() {
        let declares_test = std::fs::read_to_string(&pkg)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("scripts").and_then(|s| s.get("test")).cloned())
            .is_some();
        if declares_test {
            if worktree_path.join("pnpm-lock.yaml").exists() {
                return Some(("pnpm".into(), vec!["test".into()]));
            }
            return Some(("npm".into(), vec!["test".into()]));
        }
    }
    // Python: a pyproject.toml or a pytest config → `pytest`.
    if worktree_path.join("pyproject.toml").exists()
        || worktree_path.join("pytest.ini").exists()
        || worktree_path.join("setup.cfg").exists()
    {
        return Some(("pytest".into(), vec![]));
    }
    None
}

/// Run the detected test command in `worktree_path` (bounded by
/// [`VERIFY_TIMEOUT`]) and return a one-line observation. Best-effort: a missing
/// harness, a spawn failure, or a timeout each degrade to a note rather than an
/// error so verification never sinks an otherwise-good candidate.
async fn verify_worktree(worktree_path: &Path) -> String {
    let Some((program, args)) = detect_test_command(worktree_path) else {
        return "no test harness detected".to_string();
    };
    let path = worktree_path.to_path_buf();
    let cmd_label = format!("{program} {}", args.join(" ")).trim().to_string();
    let join = tokio::task::spawn_blocking(move || {
        crate::sys::no_window(&program)
            .args(&args)
            .current_dir(&path)
            .output()
    });
    match tokio::time::timeout(VERIFY_TIMEOUT, join).await {
        Ok(Ok(Ok(out))) if out.status.success() => format!("`{cmd_label}` PASSED"),
        Ok(Ok(Ok(out))) => {
            let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            format!("`{cmd_label}` FAILED (exit {code})")
        }
        Ok(Ok(Err(e))) => format!("`{cmd_label}` could not run: {e}"),
        Ok(Err(e)) => format!("`{cmd_label}` task panicked: {e}"),
        Err(_) => format!("`{cmd_label}` timed out after {}s", VERIFY_TIMEOUT.as_secs()),
    }
}

/// Ask a strong model to SELECT the single best candidate diff (by 1-based
/// index). Returns the chosen index when it can be parsed, else `None` (callers
/// fall back to a deterministic heuristic). This is SELECTION, not a merge:
/// programmatic conflict-resolving diff-merge is out of scope for v1, so we keep
/// exactly one candidate's branch and discard the rest.
fn build_select_prompt(task: &str, candidates: &[CodeCandidate]) -> String {
    let mut sections = String::new();
    for (i, c) in candidates.iter().enumerate() {
        sections.push_str(&format!(
            "--- candidate {} (model {}, branch {}) ---\nsummary: {}\n\ndiff:\n{}\n\n",
            i + 1,
            c.model,
            c.branch,
            c.summary.trim(),
            c.diff.trim(),
        ));
    }
    format!(
        "You are a critical code reviewer. Several models independently attempted \
         the SAME coding subtask, each in its own git worktree; their diffs are \
         below. Choose the SINGLE best candidate — the one most correct, complete, \
         and least risky. Reply with ONLY the candidate number (e.g. `2`), nothing \
         else.\n\n\
         Subtask: {task}\n\n\
         Candidates:\n\n{sections}\
         Best candidate number:"
    )
}

/// Parse the selector's reply into a 0-based candidate index, tolerating prose
/// around the number ("Candidate 2 is best" → 1). Out-of-range / unparseable →
/// `None`.
fn parse_selected_index(raw: &str, n: usize) -> Option<usize> {
    let digits: String = raw.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let one_based: usize = digits.parse().ok()?;
    if one_based >= 1 && one_based <= n {
        Some(one_based - 1)
    } else {
        None
    }
}

/// Build the AI-AUTO-MERGE prompt: ask a strong model to integrate the BEST of
/// every candidate diff into ONE unified diff. Unlike [`build_select_prompt`]
/// (which asks for a single index) this asks the model to actually combine the
/// changes — resolve overlaps/conflicts, keep the strongest of each — and emit a
/// single valid `git diff`/patch and nothing else. The caller strips code fences
/// and `git apply`s the result, so the contract is "raw unified diff only".
fn build_merge_prompt(goal: &str, task: &str, candidates: &[CodeCandidate]) -> String {
    let mut sections = String::new();
    for (i, c) in candidates.iter().enumerate() {
        sections.push_str(&format!(
            "--- candidate {} (model {}) ---\nsummary: {}\n\ndiff:\n{}\n\n",
            i + 1,
            c.model,
            c.summary.trim(),
            c.diff.trim(),
        ));
    }
    format!(
        "You are a critical code-merge engine. Several models independently \
         attempted the SAME coding subtask, each in its own git worktree against \
         the SAME base commit; their unified diffs are below. Produce ONE unified \
         diff that integrates the BEST of all candidates: resolve overlaps and \
         conflicts, keep the strongest/most-correct version of each hunk, and drop \
         duplicated or mistaken changes — do NOT simply pick one candidate.\n\n\
         Overall goal: {goal}\n\
         Subtask: {task}\n\n\
         Candidate diffs:\n\n{sections}\
         Output REQUIREMENTS: reply with ONLY a single valid unified diff in git \
         patch format (the kind `git apply` accepts — `diff --git` / `---` / `+++` \
         / `@@` headers and +/- lines). No prose, no explanation, no code fences. \
         The diff must apply against the same base the candidates were cut from.\n\n\
         Merged unified diff:"
    )
}

/// Strip Markdown code fences / leading prose from a model's diff reply so it is
/// ready for `git apply`. Tolerant: if the reply is fenced (```diff … ```), take
/// the fenced body; otherwise start from the first `diff --git`/`--- ` line we
/// recognize as the head of a unified diff. Returns the trimmed candidate diff,
/// which may be empty (caller treats empty as "unparseable → fall back").
fn sanitize_diff(raw: &str) -> String {
    let trimmed = raw.trim();
    // 1. Fenced block: lift the body of the first ``` … ``` pair.
    if let Some(after_open) = trimmed.find("```") {
        let body_start = trimmed[after_open + 3..]
            .find('\n')
            .map(|nl| after_open + 3 + nl + 1)
            .unwrap_or(trimmed.len());
        if let Some(close_rel) = trimmed[body_start..].find("```") {
            return trimmed[body_start..body_start + close_rel].trim_end().to_string();
        }
    }
    // 2. Unfenced: skip any prose preamble, start at the first diff header.
    for marker in ["diff --git ", "--- "] {
        if let Some(pos) = trimmed.find(marker) {
            // Only treat "--- " as a head if it's at the start of a line.
            if marker == "--- " && pos != 0 && trimmed.as_bytes().get(pos - 1) != Some(&b'\n') {
                continue;
            }
            return trimmed[pos..].trim_end().to_string();
        }
    }
    // 3. No recognizable diff structure → empty (unparseable).
    String::new()
}

/// `true` when `text` looks like a real unified diff worth handing to `git
/// apply` — has a `diff --git`/`--- ` header AND at least one hunk (`@@`).
/// Cheap, offline, deterministic — so the "merged diff empty/unparseable → fall
/// back" decision can be unit-tested without git.
fn looks_like_unified_diff(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let has_header = t.lines().any(|l| l.starts_with("diff --git ") || l.starts_with("--- "));
    let has_hunk = t.lines().any(|l| l.starts_with("@@"));
    has_header && has_hunk
}

/// Apply a unified diff to `worktree_path`, preferring a 3-way merge (which can
/// reconstruct context from blobs) and falling back to a plain `git apply`.
/// Returns `Ok(())` only when git reports success. Best-effort and offline-safe;
/// any spawn/exec failure surfaces as `Err` so the caller can fall back to
/// selection.
fn apply_diff_to_worktree(worktree_path: &Path, diff: &str) -> Result<(), String> {
    use std::io::Write;
    let run = |three_way: bool| -> Result<bool, String> {
        let mut cmd = crate::sys::no_window("git");
        cmd.arg("-C").arg(worktree_path).arg("apply");
        if three_way {
            cmd.arg("--3way");
        }
        cmd.arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("git apply spawn failed: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(diff.as_bytes())
                .map_err(|e| format!("git apply stdin failed: {e}"))?;
            // Ensure a trailing newline — git is picky about truncated patches.
            if !diff.ends_with('\n') {
                let _ = stdin.write_all(b"\n");
            }
        }
        let out = child.wait_with_output().map_err(|e| format!("git apply wait failed: {e}"))?;
        Ok(out.status.success())
    };
    // Prefer --3way (resolves context against the index/base); fall back to a
    // strict apply if 3-way isn't possible for this patch.
    if run(true)? {
        return Ok(());
    }
    if run(false)? {
        return Ok(());
    }
    Err("git apply (--3way and plain) both failed".to_string())
}

/// Decide, from the model's merge reply, whether the AUTO-MERGE attempt should
/// even be tried on disk. Pure + deterministic so the merge-vs-fallback gate is
/// unit-testable: returns the sanitized diff when it looks like a real unified
/// diff, else `None` ("empty/unparseable → fall back to selection").
fn merge_diff_or_fallback(raw_reply: &str) -> Option<String> {
    let sanitized = sanitize_diff(raw_reply);
    if looks_like_unified_diff(&sanitized) {
        Some(sanitized)
    } else {
        None
    }
}

/// Run a CODE subtask via the git-worktree path: each code-capable model edits
/// files in its own worktree and diffs are captured. With ≥2 non-empty diffs the
/// engine first attempts an AI AUTO-MERGE (combine all diffs into one unified
/// diff, apply it to a fresh worktree, verify it); if the merge can't be applied
/// or fails verification it FALLS BACK to SELECTing the single best candidate.
/// The surviving branch is verified best-effort, and every other worktree is
/// torn down.
///
/// Returns `Ok(Some(SubtaskResult, usd))` when the worktree path ran (even if
/// every model produced an empty diff — that's an honest result). Returns
/// `Ok(None)` when the path is not applicable (no code-capable model) so the
/// caller can transparently fall back to the chat path. Never leaves an orphaned
/// worktree: every created worktree is tracked and removed except the winner's.
#[allow(clippy::too_many_arguments)]
async fn run_code_subtask_worktrees(
    registry: &Arc<RwLock<Registry>>,
    ws: &WorktreeStore,
    project_root: &Path,
    goal: &str,
    subtask: &PlannedSubtask,
    local_tags: &[String],
    fan_out: usize,
    emit: &(impl Fn(UltEvent) + Send + Sync),
) -> Result<Option<(SubtaskResult, f64)>, String> {
    // 1. Choose code-capable models. `cost_router::candidates` already filters to
    //    adapters advertising CodeEdit (+ the gateway/CLI that also do ShellExec),
    //    and each pick carries the agent_id we route to. No code-capable model →
    //    signal the caller to fall back to chat (NOT a failure).
    let code_caps = [AgentCapability::Chat, AgentCapability::CodeEdit];
    let picks = {
        let reg = registry.read();
        cost_router::candidates(&code_caps, &reg, local_tags)
    };
    if picks.is_empty() {
        return Ok(None);
    }
    // Diverse, distinct set up to fan_out: keep order but dedup by slug.
    let mut chosen: Vec<cost_router::ModelPick> = Vec::new();
    for p in picks {
        if chosen.iter().any(|c| c.model == p.model) {
            continue;
        }
        chosen.push(p);
        if chosen.len() >= fan_out.max(1) {
            break;
        }
    }

    emit(UltEvent::SubtaskStarted {
        id: subtask.id.clone(),
        task: subtask.task.clone(),
        models: chosen.iter().map(|c| c.model.clone()).collect(),
    });

    let prompt = build_code_prompt(goal, &subtask.task);

    // 2. Run each model in its own worktree, in parallel (bounded by `chosen`).
    //    Each branch creates AT MOST one worktree and always records its id so a
    //    guard can clean it up; on any per-candidate error we tear that worktree
    //    down immediately so a mid-flight failure can't orphan it.
    let runs = chosen.iter().map(|pick| {
        let prompt = prompt.clone();
        async move {
            let wt = match worktrees::create_worktree(
                ws,
                project_root,
                Some(format!("ultimate:{}:{}", subtask.id, pick.model)),
            ) {
                Ok(w) => w,
                Err(e) => {
                    return Err(format!("worktree create failed for {}: {e}", pick.model));
                }
            };
            let path = PathBuf::from(&wt.path);
            let adapter = match registry.read().get(&pick.agent_id) {
                Some(a) => a,
                None => {
                    // Roll back the just-created worktree before bailing.
                    let _ = worktrees::remove_worktree(ws, &wt.id, false);
                    return Err(format!("adapter `{}` not registered", pick.agent_id));
                }
            };

            let run = run_adapter_in_worktree(adapter, &pick.model, &path, &prompt).await;
            let summary = match run {
                Ok(text) => text.trim().to_string(),
                Err(e) => {
                    // Capture whatever the model managed to write before erroring,
                    // but note the failure in the summary.
                    format!("(run error: {e})")
                }
            };
            let diff = capture_worktree_diff(&path);
            let ok = !diff.trim().is_empty();
            let usd = projected_usd(&prompt, &summary, Some(&pick.model));
            Ok(CodeCandidate {
                model: pick.model.clone(),
                worktree_id: wt.id,
                branch: wt.branch,
                path,
                diff,
                summary,
                ok,
                usd,
            })
        }
    });

    let mut candidates: Vec<CodeCandidate> = Vec::new();
    let mut subtask_usd = 0.0;
    for res in futures::future::join_all(runs).await {
        match res {
            Ok(c) => {
                subtask_usd += c.usd;
                emit(UltEvent::ModelDone {
                    subtask_id: subtask.id.clone(),
                    model: c.model.clone(),
                    ok: c.ok,
                    output: format!("{} — {}", diff_stat(&c.diff), c.summary),
                });
                candidates.push(c);
            }
            Err(e) => {
                // A creation/adapter failure already cleaned up its own worktree.
                emit(UltEvent::ModelDone {
                    subtask_id: subtask.id.clone(),
                    model: "<unknown>".into(),
                    ok: false,
                    output: e,
                });
            }
        }
    }

    if candidates.is_empty() {
        // Every model failed before producing a candidate → fall back to chat so
        // the subtask still has a shot rather than silently dying.
        return Ok(None);
    }

    // Guard: if we panic/early-return past here, tear down EVERY remaining
    // worktree. We disarm it for the winner at the end. Implemented as an
    // explicit "to clean" id list rather than RAII because cleanup is fallible
    // git work we want to run on the happy path too.
    let all_ids: Vec<String> = candidates.iter().map(|c| c.worktree_id.clone()).collect();

    // 3. Pick the SELECTION winner first — this is the deterministic fallback
    //    target AND, for ≤1 non-empty diff, the only sensible result. With ≥2
    //    non-empty diffs we ask a strong model to pick by index; a selector
    //    hiccup degrades to the biggest non-empty diff as a cheap proxy.
    let non_empty: Vec<usize> =
        candidates.iter().enumerate().filter(|(_, c)| c.ok).map(|(i, _)| i).collect();
    let winner_idx = if non_empty.len() >= 2 {
        let select_prompt = build_select_prompt(&subtask.task, &candidates);
        let sel_model = cost_router::pick_model_for(
            Difficulty::Hard,
            &[AgentCapability::Chat],
            &registry.read(),
            local_tags,
        )
        .map(|p| p.model);
        match oneshot::complete_resilient(registry, sel_model.clone(), select_prompt.clone()).await {
            Ok(o) => {
                subtask_usd += projected_usd(&select_prompt, &o.text, sel_model.as_deref());
                parse_selected_index(&o.text, candidates.len())
                    .filter(|i| candidates[*i].ok)
                    // Selector hiccup → biggest non-empty diff as a cheap proxy.
                    .unwrap_or_else(|| {
                        *non_empty.iter().max_by_key(|i| candidates[**i].diff.len()).unwrap()
                    })
            }
            Err(_) => *non_empty.iter().max_by_key(|i| candidates[**i].diff.len()).unwrap(),
        }
    } else if let Some(&i) = non_empty.first() {
        i
    } else {
        // No diffs at all — keep the first candidate (its prose may still help)
        // but treat the subtask as not having truly edited anything.
        0
    };

    // 4. AI AUTO-MERGE (only meaningful with ≥2 non-empty diffs to integrate).
    //    Ask a strong model for ONE unified diff combining the best of all
    //    candidates, apply it to a FRESH merge worktree cut from the same base,
    //    and verify it. On success this REPLACES the selection winner; on any
    //    failure (no diff returned, unparseable, won't apply, fails verify) we
    //    fall back to the selection winner above and tear the merge worktree
    //    down. `merge_outcome` carries the resolved branch/path/etc. either way.
    struct MergeOutcome {
        /// `true` when the auto-merge succeeded and is the chosen result.
        auto_merged: bool,
        branch: String,
        path: PathBuf,
        diff: String,
        /// Worktree id of the CHOSEN result (merge or selection winner) — kept.
        keep_id: String,
        /// Worktree id of the merge worktree IF one was created (so a fallback
        /// can tear it down). `None` when no merge worktree exists.
        merge_id: Option<String>,
        /// Human-readable description of what happened (for the event + note).
        note: String,
        /// `true` when the chosen result actually edits something on disk.
        any_edit: bool,
        /// Best-effort verification note for the chosen result.
        verify_note: String,
        /// Prose summary to thread into the subtask output.
        summary: String,
        /// Model attribution for the subtask output.
        model: String,
    }

    let merge_outcome: MergeOutcome = if non_empty.len() >= 2 {
        // 4a. Create a fresh merge worktree from the base (same mechanism).
        let merge_wt = worktrees::create_worktree(
            ws,
            project_root,
            Some(format!("ultimate:{}:merge", subtask.id)),
        );

        // 4b. Ask a strong model for a unified merged diff.
        let merge_prompt = build_merge_prompt(goal, &subtask.task, &candidates);
        let merge_model = cost_router::pick_model_for(
            Difficulty::Hard,
            &[AgentCapability::Chat],
            &registry.read(),
            local_tags,
        )
        .map(|p| p.model);

        // Track the merge worktree id the instant it's created so EVERY failure
        // arm can tear it down — no orphan if the model errors or the patch
        // won't apply. Closure-free; each early bail records WHY we fell back so
        // the emitted note is honest about the merge having been tried.
        let mut stray_merge_id: Option<String> = None;
        let attempt: Result<(Worktree, String), String> = match merge_wt {
            Ok(wt) => {
                stray_merge_id = Some(wt.id.clone());
                match oneshot::complete_resilient(registry, merge_model.clone(), merge_prompt.clone())
                    .await
                {
                    Ok(o) => {
                        subtask_usd += projected_usd(&merge_prompt, &o.text, merge_model.as_deref());
                        // 4c. Sanitize + sanity-check the diff (empty/unparseable
                        //     → fall back).
                        match merge_diff_or_fallback(&o.text) {
                            Some(diff) => {
                                // 4d. Apply it (`git apply --3way`, fallback plain).
                                let mpath = PathBuf::from(&wt.path);
                                match apply_diff_to_worktree(&mpath, &diff) {
                                    Ok(()) => Ok((wt, diff)),
                                    Err(e) => Err(format!("merge diff failed to apply: {e}")),
                                }
                            }
                            None => Err("merge model returned no valid unified diff".to_string()),
                        }
                    }
                    Err(e) => Err(format!("merge model errored: {e}")),
                }
            }
            Err(e) => Err(format!("merge worktree create failed: {e}")),
        };

        match attempt {
            Ok((wt, diff)) => {
                // 4e. VERIFY the merge worktree. The merge stands only when the
                //     harness passes (or there's no harness to run).
                let mpath = PathBuf::from(&wt.path);
                // Re-capture the diff from disk so the recorded diff reflects what
                // actually landed (3-way apply may normalize it).
                let landed = {
                    let d = capture_worktree_diff(&mpath);
                    if d.trim().is_empty() { diff.clone() } else { d }
                };
                let verify_note = verify_worktree(&mpath).await;
                let verify_ok =
                    verify_note.contains("PASSED") || verify_note == "no test harness detected";
                if verify_ok {
                    // Merge wins → it's the KEPT worktree, not a stray to clean.
                    let merge_keep = wt.id.clone();
                    MergeOutcome {
                        auto_merged: true,
                        branch: wt.branch.clone(),
                        path: mpath,
                        diff: landed.clone(),
                        keep_id: merge_keep.clone(),
                        merge_id: Some(merge_keep),
                        note: format!(
                            "AUTO-MERGED {} candidate diffs into one ({}); verification: {verify_note}",
                            non_empty.len(),
                            diff_stat(&landed)
                        ),
                        any_edit: !landed.trim().is_empty(),
                        verify_note,
                        summary: format!(
                            "Auto-merged the best of {} candidate diffs into one combined change.",
                            non_empty.len()
                        ),
                        model: "auto-merge".to_string(),
                    }
                } else {
                    // Verification FAILED → discard the merge worktree, fall back.
                    let _ = worktrees::remove_worktree(ws, &wt.id, false);
                    let wm = &candidates[winner_idx];
                    let vn = verify_worktree(&wm.path).await;
                    MergeOutcome {
                        auto_merged: false,
                        branch: wm.branch.clone(),
                        path: wm.path.clone(),
                        diff: wm.diff.clone(),
                        keep_id: wm.worktree_id.clone(),
                        merge_id: None,
                        note: format!(
                            "merge attempted but verification FAILED ({verify_note}); \
                             FELL BACK to selected candidate from {} on branch `{}` ({})",
                            wm.model,
                            wm.branch,
                            diff_stat(&wm.diff)
                        ),
                        any_edit: wm.ok,
                        verify_note: vn,
                        summary: wm.summary.clone(),
                        model: wm.model.clone(),
                    }
                }
            }
            Err(reason) => {
                // Merge could not be produced/applied → fall back to selection.
                // Hand the (possibly-created) merge worktree id to `merge_id` so
                // the cleanup below tears it down — no orphan on this path.
                let wm = &candidates[winner_idx];
                let vn = verify_worktree(&wm.path).await;
                MergeOutcome {
                    auto_merged: false,
                    branch: wm.branch.clone(),
                    path: wm.path.clone(),
                    diff: wm.diff.clone(),
                    keep_id: wm.worktree_id.clone(),
                    merge_id: stray_merge_id,
                    note: format!(
                        "merge attempted but {reason}; FELL BACK to selected candidate \
                         from {} on branch `{}` ({})",
                        wm.model,
                        wm.branch,
                        diff_stat(&wm.diff)
                    ),
                    any_edit: wm.ok,
                    verify_note: vn,
                    summary: wm.summary.clone(),
                    model: wm.model.clone(),
                }
            }
        }
    } else {
        // ≤1 non-empty diff: nothing to merge. The selection winner is the
        // result (its prose may still help even with an empty diff).
        let wm = &candidates[winner_idx];
        let verify_note = if wm.ok {
            verify_worktree(&wm.path).await
        } else {
            "skipped (no on-disk change)".to_string()
        };
        MergeOutcome {
            auto_merged: false,
            branch: wm.branch.clone(),
            path: wm.path.clone(),
            diff: wm.diff.clone(),
            keep_id: wm.worktree_id.clone(),
            merge_id: None,
            note: format!(
                "selected candidate from {} on branch `{}` ({})",
                wm.model,
                wm.branch,
                diff_stat(&wm.diff)
            ),
            any_edit: wm.ok,
            verify_note,
            summary: wm.summary.clone(),
            model: wm.model.clone(),
        }
    };

    let MergeOutcome {
        auto_merged,
        branch: result_branch,
        path: _result_path,
        diff: result_diff,
        keep_id,
        merge_id,
        note,
        any_edit,
        verify_note,
        summary: result_summary,
        model: result_model,
    } = merge_outcome;

    // 5. Emit the merge/fallback decision so the UI can show what happened.
    emit(UltEvent::SubtaskMerged {
        id: subtask.id.clone(),
        merged: format!("{note}; verification: {verify_note}"),
    });

    // 6. CLEANUP: remove EVERY worktree except the chosen result's (`keep_id`).
    //    This covers all candidate worktrees AND any merge worktree we created
    //    (whether the merge won or we fell back) — no orphans on any path.
    //    `remove_worktree` is best-effort (logs + archives), so a failure here
    //    can't strand the run.
    let mut cleanup_ids: Vec<String> = all_ids.clone();
    if let Some(mid) = merge_id {
        if mid != keep_id && !cleanup_ids.contains(&mid) {
            cleanup_ids.push(mid);
        }
    }
    for id in &cleanup_ids {
        if id == &keep_id {
            continue;
        }
        let _ = worktrees::remove_worktree(ws, id, false);
    }

    // 7. Thread the surviving branch + diff + verification into the result so the
    //    synthesis references the ACTUAL branch the user can check out. The
    //    header makes the auto-merge-vs-fallback outcome explicit.
    let header = if auto_merged {
        format!("AUTO-MERGED {} candidate diffs in a git worktree.", non_empty.len())
    } else {
        format!("Implemented by model `{result_model}` in a git worktree (merge fallback).")
    };
    let output = format!(
        "{header}\n\
         Branch to review/merge: `{result_branch}`\n\
         Change: {}\n\n\
         Summary: {result_summary}\n\n\
         Outcome: {note}\n\
         Verification: {verify_note}\n\n\
         Diff:\n```diff\n{}\n```",
        diff_stat(&result_diff),
        result_diff.trim(),
    );

    let result = SubtaskResult {
        id: subtask.id.clone(),
        task: subtask.task.clone(),
        models: candidates.iter().map(|c| c.model.clone()).collect(),
        output,
        ok: any_edit,
    };
    Ok(Some((result, subtask_usd)))
}

/// Build the critical-aggregator prompt that MERGES fan-out candidates. It is
/// explicit that this is a merge — resolve conflicts, keep the strongest of
/// each — NOT a vote / pick-one.
fn build_aggregate_prompt(task: &str, candidates: &[(String, String)]) -> String {
    let mut sections = String::new();
    for (model, output) in candidates {
        sections.push_str(&format!("--- candidate from {model} ---\n{}\n\n", output.trim()));
    }
    format!(
        "You are a critical aggregator. Several models independently attempted the \
         same subtask. Merge their candidate outputs into ONE best result: resolve \
         contradictions, keep the strongest parts of each, and correct mistakes — \
         do NOT simply pick one candidate or take a vote.\n\n\
         Subtask: {task}\n\n\
         Candidates:\n\n{sections}\
         Produce the single merged result now."
    )
}

/// Build the final synthesis + verification prompt over every subtask result
/// (shape mirrors `team_run::build_synthesis_prompt`).
fn build_synthesis_prompt(goal: &str, results: &[SubtaskResult]) -> String {
    let mut sections = String::new();
    for r in results {
        sections.push_str(&format!(
            "--- {} (models: {}) ---\n{}\n\n",
            r.id,
            r.models.join(", "),
            r.output.trim()
        ));
    }
    format!(
        "You are the lead orchestrator. The subtasks toward this goal have been \
         completed (some raced across multiple models and already merged).\n\n\
         Goal: {goal}\n\n\
         Subtask results:\n\n{sections}\
         Produce ONE coherent merged deliverable that accomplishes the goal by \
         integrating the subtask results — resolve overlaps, keep the strongest of \
         each, reconcile contradictions. Then add a section headed \"## Verification\": \
         state whether the pieces are mutually consistent and whether the goal is \
         fully met, call out any subtask that failed or left a gap, and list concrete \
         follow-ups if work remains. Be specific and do not claim work that was not done."
    )
}

/// Record a (prompt, outcome) pair as a fresh chat session and return its id —
/// same materialize-as-session shape `team_run::record_transcript` uses.
fn record_transcript(
    store: &TracingStore,
    title: &str,
    prompt: &str,
    outcome: &str,
    run_id: &str,
) -> anyhow::Result<String> {
    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let user = StoredMessage {
        id: format!("ult-u-{}", uuid::Uuid::new_v4()),
        session_id: session_id.clone(),
        ts,
        role: "user".into(),
        agent_id: None,
        content: format!("{title}\n\n{prompt}"),
        run_id: Some(run_id.to_string()),
        reasoning: None,
        project_root: None,
    };
    let assistant = StoredMessage {
        id: format!("ult-a-{}", uuid::Uuid::new_v4()),
        ts: ts + 1,
        role: "assistant".into(),
        content: outcome.to_string(),
        ..user.clone()
    };
    store.record_message(&user)?;
    store.record_message(&assistant)?;
    Ok(session_id)
}

/// Run the ultimate hybrid orchestrator end-to-end. See the module docs for the
/// pipeline. Tauri-free: streams progress through `emit` and returns the result.
pub async fn run_ultimate(
    registry: Arc<RwLock<Registry>>,
    store: TracingStore,
    cfg: UltimateConfig,
    emit: impl Fn(UltEvent) + Send + Sync,
) -> Result<UltimateResult, String> {
    let goal = cfg.goal.trim().to_string();
    if goal.is_empty() {
        emit(UltEvent::Error { msg: "empty goal".into() });
        return Err("empty goal".into());
    }
    let fan_out = cfg.fan_out.max(1);
    let run_id = format!("ultimate:{}", uuid::Uuid::new_v4());

    // ── Discover the model roster ─────────────────────────────────────────
    let roster = discover_models(&registry).await;
    if roster.is_empty() {
        emit(UltEvent::Error { msg: "no models available".into() });
        return Err("no models available".into());
    }
    let local_tags: Vec<String> =
        roster.iter().filter(|m| m.starts_with("ollama:")).cloned().collect();

    // Lead model: pinned, else the strongest capable chat pick, else roster[0].
    let lead_model: Option<String> = cfg
        .lead_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            cost_router::pick_model_for(
                Difficulty::Hard,
                &[AgentCapability::Chat],
                &registry.read(),
                &local_tags,
            )
            .map(|p| p.model)
        })
        .or_else(|| roster.first().cloned());

    let mut total_usd: f64 = 0.0;

    // ── DECOMPOSE ─────────────────────────────────────────────────────────
    let plan_prompt = build_plan_prompt(&goal, &roster);
    let plan_raw =
        match oneshot::complete_resilient(&registry, lead_model.clone(), plan_prompt.clone()).await
        {
            Ok(o) => {
                total_usd += projected_usd(&plan_prompt, &o.text, lead_model.as_deref());
                o.text
            }
            Err(e) => {
                emit(UltEvent::Error { msg: format!("lead could not plan: {e}") });
                return Err(format!("lead could not plan: {e}"));
            }
        };
    let _ = record_transcript(&store, "Ultimate — lead plan", &plan_prompt, &plan_raw, &run_id);
    let subtasks = parse_plan(&plan_raw, &goal);
    emit(UltEvent::Plan { subtasks: subtasks.clone() });

    // Code subtasks take the git-worktree path when a valid git `project_root`
    // is configured. We open a `WorktreeStore` over the SAME sqlite connection
    // the tracing store uses (its schema already owns the `worktrees` table) —
    // mirrors `commands/worktrees::store_from`, so the public signature is
    // unchanged. `None` (or a non-git root) leaves every subtask on the chat path.
    let worktree_store = WorktreeStore::new(store.shared_connection());
    let code_root: Option<PathBuf> = cfg
        .project_root
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| is_git_repo(p));

    // ── EXECUTE each subtask concurrently ─────────────────────────────────
    let registry_ref = &registry;
    let store_ref = &store;
    let goal_ref = goal.as_str();
    let run_id_ref = run_id.as_str();
    let roster_ref = &roster;
    let local_tags_ref = &local_tags;
    let emit_ref = &emit;
    let worktree_store_ref = &worktree_store;
    let code_root_ref = &code_root;

    let runs = subtasks.iter().map(|st| async move {
        // CODE subtask + a valid git project root → the worktree path: each model
        // edits files in its own worktree, the best diff is selected + verified.
        // It returns `None` when not applicable (no code-capable model / every
        // model failed pre-diff), in which case we transparently fall through to
        // the chat path below so the subtask is never stranded.
        if task_kind(st) == TaskKind::Code {
            if let Some(root) = code_root_ref.as_deref() {
                match run_code_subtask_worktrees(
                    registry_ref,
                    worktree_store_ref,
                    root,
                    goal_ref,
                    st,
                    local_tags_ref,
                    fan_out,
                    emit_ref,
                )
                .await
                {
                    Ok(Some(out)) => return out,
                    // `Ok(None)` (fall back to chat) or `Err` (worktree path
                    // blew up) → degrade to the chat path rather than failing.
                    Ok(None) => {}
                    Err(e) => emit_ref(UltEvent::Error {
                        msg: format!("worktree path for {} failed, using chat: {e}", st.id),
                    }),
                }
            }
        }

        let models = select_models(registry_ref, st, roster_ref, local_tags_ref, fan_out);
        emit_ref(UltEvent::SubtaskStarted {
            id: st.id.clone(),
            task: st.task.clone(),
            models: models.clone(),
        });

        // Run each model's candidate in parallel.
        let prompt = build_worker_prompt(goal_ref, &st.task);
        let candidate_runs = models.iter().map(|m| {
            let prompt = prompt.clone();
            async move {
                let res =
                    oneshot::complete_resilient(registry_ref, Some(m.clone()), prompt.clone()).await;
                (m.clone(), prompt, res)
            }
        });
        let mut candidates: Vec<(String, String)> = Vec::new();
        let mut subtask_usd = 0.0;
        for (model, prompt, res) in futures::future::join_all(candidate_runs).await {
            match res {
                Ok(o) => {
                    subtask_usd += projected_usd(&prompt, &o.text, Some(&model));
                    emit_ref(UltEvent::ModelDone {
                        subtask_id: st.id.clone(),
                        model: model.clone(),
                        ok: true,
                        output: o.text.clone(),
                    });
                    candidates.push((model, o.text.trim().to_string()));
                }
                Err(e) => {
                    // A refusal/error just drops its candidate.
                    emit_ref(UltEvent::ModelDone {
                        subtask_id: st.id.clone(),
                        model,
                        ok: false,
                        output: e,
                    });
                }
            }
        }

        // Retry with different roster models if EVERY candidate failed. We walk
        // the remaining roster (distinct from the models already tried) until one
        // produces a candidate — a model that maps to the same wedged adapter as
        // the original pick shouldn't doom the subtask when a different brain is
        // reachable.
        if candidates.is_empty() {
            for alt in roster_ref.iter().filter(|m| !models.contains(m)) {
                if let Ok(o) =
                    oneshot::complete_resilient(registry_ref, Some(alt.clone()), prompt.clone())
                        .await
                {
                    subtask_usd += projected_usd(&prompt, &o.text, Some(alt));
                    emit_ref(UltEvent::ModelDone {
                        subtask_id: st.id.clone(),
                        model: alt.clone(),
                        ok: true,
                        output: o.text.clone(),
                    });
                    candidates.push((alt.clone(), o.text.trim().to_string()));
                    break;
                }
            }
        }

        // AGGREGATE: a fan-out subtask with ≥2 candidates is merged by a strong
        // critical-aggregator; otherwise the lone candidate stands.
        let (output, merged_via) = if st.fan_out && candidates.len() >= 2 {
            let agg_prompt = build_aggregate_prompt(&st.task, &candidates);
            let agg_model = cost_router::pick_model_for(
                Difficulty::Hard,
                &[AgentCapability::Chat],
                &registry_ref.read(),
                local_tags_ref,
            )
            .map(|p| p.model)
            .or_else(|| roster_ref.first().cloned());
            match oneshot::complete_resilient(registry_ref, agg_model.clone(), agg_prompt.clone())
                .await
            {
                Ok(o) if !o.text.trim().is_empty() => {
                    let usd = projected_usd(&agg_prompt, &o.text, agg_model.as_deref());
                    let merged = o.text.trim().to_string();
                    let _ = record_transcript(
                        store_ref,
                        &format!("Ultimate — aggregate {}", st.id),
                        &agg_prompt,
                        &merged,
                        run_id_ref,
                    );
                    emit_ref(UltEvent::SubtaskMerged { id: st.id.clone(), merged: merged.clone() });
                    (merged, usd)
                }
                // Aggregator hiccup → fall back to the strongest single candidate
                // (the longest, as a cheap proxy for the most complete).
                _ => {
                    let best = candidates
                        .iter()
                        .max_by_key(|(_, t)| t.len())
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default();
                    (best, 0.0)
                }
            }
        } else {
            let single = candidates.first().map(|(_, t)| t.clone()).unwrap_or_default();
            (single, 0.0)
        };
        subtask_usd += merged_via;

        let result = SubtaskResult {
            id: st.id.clone(),
            task: st.task.clone(),
            models,
            output,
            ok: !candidates.is_empty(),
        };
        (result, subtask_usd)
    });

    let mut subtasks_out: Vec<SubtaskResult> = Vec::new();
    for (r, usd) in futures::future::join_all(runs).await {
        total_usd += usd;
        subtasks_out.push(r);
    }
    subtasks_out.sort_by(|a, b| a.id.cmp(&b.id));
    emit(UltEvent::Cost { usd: total_usd });

    // ── SYNTHESIZE ────────────────────────────────────────────────────────
    let final_output = if subtasks_out.len() < 2 {
        // Nothing to merge — the single subtask's output IS the deliverable.
        subtasks_out.first().map(|r| r.output.clone()).unwrap_or_default()
    } else {
        let synth_prompt = build_synthesis_prompt(&goal, &subtasks_out);
        let synth_model = lead_model.clone();
        match oneshot::complete_resilient(&registry, synth_model.clone(), synth_prompt.clone()).await
        {
            Ok(o) if !o.text.trim().is_empty() => {
                total_usd += projected_usd(&synth_prompt, &o.text, synth_model.as_deref());
                let merged = o.text.trim().to_string();
                let _ = record_transcript(
                    &store,
                    "Ultimate — synthesis",
                    &synth_prompt,
                    &merged,
                    &run_id,
                );
                emit(UltEvent::Synthesis { merged: merged.clone() });
                merged
            }
            // Synthesis is best-effort: degrade to a concatenation of the
            // subtask outputs rather than failing the whole run.
            _ => subtasks_out
                .iter()
                .map(|r| format!("## {}\n{}", r.id, r.output))
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    };

    emit(UltEvent::Cost { usd: total_usd });
    let ok = !subtasks_out.is_empty() && subtasks_out.iter().all(|r| r.ok);
    emit(UltEvent::Done { ok });

    Ok(UltimateResult { final_output, subtasks: subtasks_out, total_usd })
}

/// Project the USD cost of one completion (prompt always sent; completion priced
/// only on success — callers pass the produced text). Free local → $0.
fn projected_usd(prompt: &str, completion: &str, model: Option<&str>) -> f64 {
    let price = model.map(price_for_slug).unwrap_or((0.0, 0.0));
    compute_usd(estimate_tokens(prompt), estimate_tokens(completion), price)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::{AgentAdapter, AgentDescriptor, AgentEvent, ChatRequest};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    // ── Pure helpers (deterministic, no model) ──────────────────────────────

    #[test]
    fn parse_plan_reads_clean_json() {
        let raw = r#"[
          {"id":"s1","task":"design the API","kind":"chat","difficulty":"hard","fan_out":true},
          {"id":"s2","task":"write a test","kind":"chat","difficulty":"easy","fan_out":false}
        ]"#;
        let got = parse_plan(raw, "goal");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "s1");
        assert!(got[0].fan_out);
        assert!(!got[1].fan_out);
        assert_eq!(got[0].difficulty, "hard");
    }

    #[test]
    fn parse_plan_tolerates_fences_and_synthesizes_ids() {
        let raw = "Sure!\n```json\n[{\"task\":\"do a thing\"}]\n```";
        let got = parse_plan(raw, "goal");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "s1", "missing id is synthesized");
        assert_eq!(got[0].kind, "chat", "defaults to chat");
        assert!(!got[0].fan_out, "defaults to no fan-out");
    }

    #[test]
    fn parse_plan_garbage_falls_back_to_single_goal_subtask() {
        for raw in ["no json here", "[broken", ""] {
            let got = parse_plan(raw, "ship the thing");
            assert_eq!(got.len(), 1, "raw={raw:?}");
            assert_eq!(got[0].task, "ship the thing");
        }
    }

    #[test]
    fn plan_prompt_demands_json_and_fan_out_flag() {
        let p = build_plan_prompt("ship dark mode", &["claude-opus-4-8".into()]);
        assert!(p.contains("ship dark mode"));
        assert!(p.contains("ONLY a JSON array"));
        assert!(p.contains("fan_out"));
        assert!(p.contains("claude-opus-4-8"), "roster is shown to the lead");
    }

    #[test]
    fn aggregate_prompt_demands_merge_not_vote() {
        let cands = vec![
            ("model-a".to_string(), "approach A".to_string()),
            ("model-b".to_string(), "approach B".to_string()),
        ];
        let p = build_aggregate_prompt("design the cache", &cands);
        assert!(p.contains("approach A") && p.contains("approach B"));
        assert!(p.contains("model-a") && p.contains("model-b"));
        assert!(p.to_lowercase().contains("merge"));
        assert!(p.contains("do NOT simply pick one"), "explicitly not a vote");
    }

    #[test]
    fn synthesis_prompt_carries_results_and_demands_verification() {
        let results = vec![
            SubtaskResult {
                id: "s1".into(),
                task: "t1".into(),
                models: vec!["m1".into()],
                output: "did s1".into(),
                ok: true,
            },
            SubtaskResult {
                id: "s2".into(),
                task: "t2".into(),
                models: vec!["m2".into()],
                output: "did s2".into(),
                ok: true,
            },
        ];
        let p = build_synthesis_prompt("the goal", &results);
        assert!(p.contains("the goal"));
        assert!(p.contains("did s1") && p.contains("did s2"));
        assert!(p.contains("## Verification"));
    }

    // ── Worktree code-path pure helpers (deterministic, offline) ─────────────

    #[test]
    fn diff_stat_counts_files_and_lines() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@\n+added one\n+added two\n-removed one\n";
        // 1 file; '+++' / '---' headers excluded; 2 adds, 1 remove.
        assert_eq!(diff_stat(diff), "1 file(s), +2/-1");
        assert_eq!(diff_stat(""), "0 file(s), +0/-0");
    }

    #[test]
    fn parse_selected_index_tolerates_prose_and_clamps() {
        assert_eq!(parse_selected_index("2", 3), Some(1));
        assert_eq!(parse_selected_index("Candidate 3 is best", 3), Some(2));
        assert_eq!(parse_selected_index("1", 3), Some(0));
        assert_eq!(parse_selected_index("9", 3), None, "out of range → None");
        assert_eq!(parse_selected_index("none of them", 3), None, "no digit → None");
    }

    #[test]
    fn select_prompt_demands_a_single_index_not_a_merge() {
        let cands = vec![
            CodeCandidate {
                model: "m1".into(),
                worktree_id: "w1".into(),
                branch: "cortex/aaa".into(),
                path: PathBuf::from("/tmp/w1"),
                diff: "diff --git a/f b/f\n+x".into(),
                summary: "did a".into(),
                ok: true,
                usd: 0.0,
            },
            CodeCandidate {
                model: "m2".into(),
                worktree_id: "w2".into(),
                branch: "cortex/bbb".into(),
                path: PathBuf::from("/tmp/w2"),
                diff: "diff --git a/g b/g\n+y".into(),
                summary: "did b".into(),
                ok: true,
                usd: 0.0,
            },
        ];
        let p = build_select_prompt("add a thing", &cands);
        assert!(p.contains("cortex/aaa") && p.contains("cortex/bbb"));
        assert!(p.contains("ONLY the candidate number"), "selection, not merge");
        assert!(p.to_lowercase().contains("best"));
    }

    // ── AI auto-merge helpers (deterministic, offline) ──────────────────────

    #[test]
    fn merge_prompt_demands_one_unified_diff_not_a_pick() {
        let cands = vec![
            CodeCandidate {
                model: "m1".into(),
                worktree_id: "w1".into(),
                branch: "cortex/aaa".into(),
                path: PathBuf::from("/tmp/w1"),
                diff: "diff --git a/f b/f\n@@\n+x".into(),
                summary: "did a".into(),
                ok: true,
                usd: 0.0,
            },
            CodeCandidate {
                model: "m2".into(),
                worktree_id: "w2".into(),
                branch: "cortex/bbb".into(),
                path: PathBuf::from("/tmp/w2"),
                diff: "diff --git a/g b/g\n@@\n+y".into(),
                summary: "did b".into(),
                ok: true,
                usd: 0.0,
            },
        ];
        let p = build_merge_prompt("the goal", "add a thing", &cands);
        // Shows both candidate diffs and the goal/subtask.
        assert!(p.contains("the goal") && p.contains("add a thing"));
        assert!(p.contains("a/f") && p.contains("a/g"));
        // Demands ONE unified diff and explicitly NOT a pick.
        assert!(p.to_lowercase().contains("unified diff"));
        assert!(p.contains("do NOT simply pick one"));
        assert!(p.contains("git apply"), "names the apply contract");
    }

    #[test]
    fn sanitize_diff_lifts_fenced_body_and_strips_prose() {
        // Fenced ```diff block → just the body.
        let fenced = "Sure, here you go:\n```diff\ndiff --git a/x b/x\n@@\n+a\n```\nthanks!";
        let got = sanitize_diff(fenced);
        assert!(got.starts_with("diff --git a/x b/x"));
        assert!(!got.contains("```") && !got.contains("Sure"));

        // Unfenced with a prose preamble → start at the first diff header.
        let prose = "I changed the file:\ndiff --git a/y b/y\n@@\n-b\n+c\n";
        let got = sanitize_diff(prose);
        assert!(got.starts_with("diff --git a/y b/y"));

        // Pure prose / no diff structure → empty.
        assert_eq!(sanitize_diff("I could not produce a diff."), "");
        assert_eq!(sanitize_diff(""), "");
    }

    #[test]
    fn looks_like_unified_diff_requires_header_and_hunk() {
        assert!(looks_like_unified_diff("diff --git a/x b/x\n@@ -1 +1 @@\n+a\n"));
        assert!(looks_like_unified_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n+a\n"));
        // Header but no hunk → not a usable patch.
        assert!(!looks_like_unified_diff("diff --git a/x b/x\n"));
        // Hunk but no header → not a usable patch.
        assert!(!looks_like_unified_diff("@@ -1 +1 @@\n+a\n"));
        assert!(!looks_like_unified_diff(""));
    }

    #[test]
    fn merge_diff_or_fallback_gates_on_a_valid_diff() {
        // A real unified diff (fenced) → Some(sanitized) → AUTO-MERGE attempted.
        let ok = "```diff\ndiff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n```";
        let got = merge_diff_or_fallback(ok);
        assert!(got.is_some());
        assert!(got.unwrap().starts_with("diff --git a/x b/x"));

        // Empty / prose / header-only replies → None → FALL BACK to selection.
        assert!(merge_diff_or_fallback("").is_none(), "empty → fallback");
        assert!(
            merge_diff_or_fallback("I picked candidate 2.").is_none(),
            "prose → fallback"
        );
        assert!(
            merge_diff_or_fallback("diff --git a/x b/x\n").is_none(),
            "header-only (no hunk) → fallback"
        );
    }

    #[test]
    fn detect_test_command_recognizes_each_ecosystem() {
        let tmp = tempfile::tempdir().unwrap();
        // Rust.
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        assert_eq!(
            detect_test_command(tmp.path()),
            Some(("cargo".into(), vec!["test".into()]))
        );

        // JS: package.json WITH a test script → npm test (no pnpm lock).
        let js = tempfile::tempdir().unwrap();
        std::fs::write(
            js.path().join("package.json"),
            r#"{"scripts":{"test":"jest"}}"#,
        )
        .unwrap();
        assert_eq!(detect_test_command(js.path()), Some(("npm".into(), vec!["test".into()])));

        // JS WITHOUT a test script → not detected.
        let js2 = tempfile::tempdir().unwrap();
        std::fs::write(js2.path().join("package.json"), r#"{"scripts":{"build":"x"}}"#).unwrap();
        assert_eq!(detect_test_command(js2.path()), None);

        // Python.
        let py = tempfile::tempdir().unwrap();
        std::fs::write(py.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert_eq!(detect_test_command(py.path()), Some(("pytest".into(), vec![])));

        // Nothing → None.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(detect_test_command(empty.path()), None);
    }

    // ── Stub adapters (mirroring team_run / oneshot) ────────────────────────

    /// Streams a canned answer keyed off the prompt so one adapter can play the
    /// lead (plan), workers, aggregator and synthesizer deterministically.
    /// Records each distinct model slug it was asked to run so the fan-out test
    /// can assert ≥2 DISTINCT models were invoked.
    struct StubAdapter {
        id: &'static str,
        plan: Option<&'static str>,
        seen_models: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.id.to_string(),
                label: self.id.to_string(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat, AgentCapability::LongContext],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
            if let Some(m) = req.model.clone() {
                self.seen_models.lock().push(m);
            }
            let msg = &req.message;
            let body: String = if msg.contains("ONLY a JSON array") {
                self.plan.unwrap_or("[]").to_string()
            } else if msg.contains("critical aggregator") {
                "MERGED: the strongest parts of every candidate, conflicts resolved.".into()
            } else if msg.contains("## Verification") {
                "FINAL DELIVERABLE.\n\n## Verification\nConsistent; goal met.".into()
            } else {
                // A worker answer; include the model so candidates differ a bit.
                format!(
                    "Candidate answer from {} for the subtask, substantial enough to merge.",
                    req.model.as_deref().unwrap_or("default")
                )
            };
            let _ = tx.send(AgentEvent::Token { delta: body }).await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            Ok(())
        }
    }

    /// A registry whose default route resolves to whatever adapter the model
    /// slug names — we register the two cloud-CLI ids the catalog routes to
    /// (claude-cli, gateway-remote) so distinct model slugs reach distinct
    /// adapters, and assert distinctness via the recorded slugs.
    ///
    /// Simpler: register ONE adapter per id we want reachable. `orchestrator::route`
    /// routes `claude-*` → `claude-cli` and the rest → `gateway-remote`.
    fn registry_two_models(
        seen: Arc<parking_lot::Mutex<Vec<String>>>,
        plan: &'static str,
    ) -> Arc<RwLock<Registry>> {
        let mut r = Registry::new();
        r.register(Arc::new(StubAdapter {
            id: "claude-cli",
            plan: Some(plan),
            seen_models: seen.clone(),
        }));
        r.register(Arc::new(StubAdapter {
            id: "gateway-remote",
            plan: Some(plan),
            seen_models: seen,
        }));
        Arc::new(RwLock::new(r))
    }

    #[test]
    fn select_models_fans_out_to_distinct_models() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let reg = registry_two_models(seen, "[]");
        let st = PlannedSubtask {
            id: "s1".into(),
            task: "hard thing".into(),
            kind: "chat".into(),
            difficulty: "hard".into(),
            fan_out: true,
        };
        // Roster drawn from the catalog candidates (claude-cli + gateway-remote
        // serve several models). Fan out to 3 → must pick ≥2 DISTINCT slugs.
        let roster = vec![
            "claude-opus-4-8".to_string(),
            "gemini-3.1-pro-preview".to_string(),
            "claude-sonnet-4-6".to_string(),
        ];
        let picked = select_models(&reg, &st, &roster, &[], 3);
        assert!(picked.len() >= 2, "fan-out must pick ≥2 models, got {picked:?}");
        let distinct: std::collections::HashSet<_> = picked.iter().collect();
        assert_eq!(distinct.len(), picked.len(), "models must be distinct: {picked:?}");
    }

    #[test]
    fn select_models_non_fanout_picks_single() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let reg = registry_two_models(seen, "[]");
        let st = PlannedSubtask {
            id: "s1".into(),
            task: "easy thing".into(),
            kind: "chat".into(),
            difficulty: "easy".into(),
            fan_out: false,
        };
        let roster = vec!["claude-opus-4-8".to_string()];
        let picked = select_models(&reg, &st, &roster, &[], 3);
        assert_eq!(picked.len(), 1, "non-fan-out picks exactly one model");
    }

    /// THE core test: a fan-out subtask runs across ≥2 DISTINCT models and the
    /// aggregator produces a merged output. Tauri-free, deterministic, no net.
    #[tokio::test]
    async fn fanout_subtask_invokes_multiple_models_and_merges() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        // Lead plan: a single fan-out chat subtask.
        let plan = r#"[{"id":"s1","task":"design the thing","kind":"chat","difficulty":"hard","fan_out":true}]"#;
        let reg = registry_two_models(seen.clone(), plan);
        let store = TracingStore::in_memory();

        let events = Arc::new(parking_lot::Mutex::new(Vec::<UltEvent>::new()));
        let ev = events.clone();
        let cfg = UltimateConfig {
            goal: "build a thing".into(),
            project_root: None,
            fan_out: 3,
            // Pin a model so the lead routes deterministically to claude-cli.
            lead_model: Some("claude-opus-4-8".into()),
        };

        let result = run_ultimate(reg, store, cfg, move |e| ev.lock().push(e))
            .await
            .expect("ultimate run completes");

        // ≥2 DISTINCT models were invoked for the fan-out subtask's candidates.
        let seen_models = seen.lock().clone();
        let worker_models: std::collections::HashSet<_> = seen_models
            .iter()
            .filter(|m| !m.is_empty())
            .cloned()
            .collect();
        assert!(
            worker_models.len() >= 2,
            "fan-out must invoke ≥2 distinct models, saw {worker_models:?}"
        );

        // The aggregator produced a merged output for the subtask.
        let evs = events.lock().clone();
        let merged = evs.iter().find_map(|e| match e {
            UltEvent::SubtaskMerged { id, merged } if id == "s1" => Some(merged.clone()),
            _ => None,
        });
        let merged = merged.expect("a SubtaskMerged event for s1");
        assert!(merged.contains("MERGED"), "aggregator output present: {merged}");

        // The run produced a final deliverable and reported Done{ok:true}.
        assert!(!result.final_output.is_empty());
        assert_eq!(result.subtasks.len(), 1);
        assert!(result.subtasks[0].models.len() >= 2);
        assert!(result.subtasks[0].ok);
        assert!(evs.iter().any(|e| matches!(e, UltEvent::Done { ok: true })));
        assert!(evs.iter().any(|e| matches!(e, UltEvent::Plan { .. })));
    }

    /// A subtask whose every candidate model errors retries once on a different
    /// roster model, and the run still completes with that fallback's output.
    #[tokio::test]
    async fn all_candidates_failing_retries_on_a_different_model() {
        // Adapter that errors for one id and answers for another.
        struct PickyAdapter {
            id: &'static str,
            fail: bool,
            calls: Arc<AtomicUsize>,
            plan: &'static str,
        }
        #[async_trait]
        impl AgentAdapter for PickyAdapter {
            fn descriptor(&self) -> AgentDescriptor {
                AgentDescriptor {
                    id: self.id.to_string(),
                    label: self.id.to_string(),
                    description: String::new(),
                    capabilities: vec![AgentCapability::Chat],
                    available: true,
                }
            }
            async fn health_check(&self) -> bool {
                true
            }
            async fn run(
                &self,
                req: ChatRequest,
                tx: mpsc::Sender<AgentEvent>,
            ) -> anyhow::Result<()> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if req.message.contains("ONLY a JSON array") {
                    let _ = tx.send(AgentEvent::Token { delta: self.plan.to_string() }).await;
                } else if self.fail {
                    let _ = tx
                        .send(AgentEvent::Error { message: "401 unauthorized".into() })
                        .await;
                } else {
                    let _ = tx
                        .send(AgentEvent::Token {
                            delta: "fallback model produced a usable answer.".into(),
                        })
                        .await;
                }
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                Ok(())
            }
        }

        // Single non-fan-out subtask; claude-cli (the routed model) fails, the
        // gateway-remote fallback in the roster answers.
        let plan = r#"[{"id":"s1","task":"do it","kind":"chat","difficulty":"hard","fan_out":false}]"#;
        let mut r = Registry::new();
        r.register(Arc::new(PickyAdapter {
            id: "claude-cli",
            fail: true,
            calls: Arc::new(AtomicUsize::new(0)),
            plan,
        }));
        r.register(Arc::new(PickyAdapter {
            id: "gateway-remote",
            fail: false,
            calls: Arc::new(AtomicUsize::new(0)),
            plan,
        }));
        let reg = Arc::new(RwLock::new(r));
        let store = TracingStore::in_memory();

        let cfg = UltimateConfig {
            goal: "accomplish".into(),
            project_root: None,
            fan_out: 1,
            lead_model: Some("gateway-remote".into()), // a model the gateway serves
        };
        let result = run_ultimate(reg, store, cfg, |_| {}).await.expect("run completes");
        assert_eq!(result.subtasks.len(), 1);
        assert!(
            result.subtasks[0].output.contains("fallback model"),
            "retry on a different model recovered: {:?}",
            result.subtasks[0].output
        );
        assert!(result.subtasks[0].ok);
    }

    /// LIVE/ignored: the git-worktree CODE path end-to-end. Needs a real `git`
    /// and sets the process-global `HOME`, so it's `#[ignore]`d like team_run's
    /// HOME-touching tests (run with `cargo test --lib -- --ignored worktree`).
    ///
    /// A fake CODE-capable adapter (registered as `claude-cli`, so the catalog
    /// routes `claude-*` slugs to it AND `cost_router::candidates` sees a CodeEdit
    /// model) WRITES A FILE into its `req.project_root` — i.e. inside the worktree
    /// the engine handed it. We then assert the diff was captured, a winner branch
    /// was selected + survived cleanup, and the loser worktree was torn down.
    #[tokio::test]
    #[ignore]
    async fn code_subtask_runs_in_worktrees_captures_diff_and_selects() {
        use std::path::Path;

        // ── A throwaway git repo to act as the project root. ────────────────
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let project = home.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let git = |args: &[&str]| {
            crate::sys::no_window("git").arg("-C").arg(&project).args(args).output().unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        let _ = git(&["config", "user.name", "test"]);
        let _ = git(&["config", "user.email", "test@local"]);
        std::fs::write(project.join("README.md"), "seed\n").unwrap();
        let _ = git(&["add", "-A"]);
        assert!(git(&["commit", "-q", "-m", "seed"]).status.success());

        // ── A code-capable adapter that edits a file in its cwd. ─────────────
        // Distinct per-model filenames so two candidates produce distinct diffs
        // (forcing the selector to choose). For the lead/plan/selector prompts it
        // falls back to chat-style text.
        struct CodeAdapter {
            id: &'static str,
            plan: &'static str,
        }
        #[async_trait]
        impl AgentAdapter for CodeAdapter {
            fn descriptor(&self) -> AgentDescriptor {
                AgentDescriptor {
                    id: self.id.to_string(),
                    label: self.id.to_string(),
                    description: String::new(),
                    capabilities: vec![
                        AgentCapability::Chat,
                        AgentCapability::CodeEdit,
                        AgentCapability::ShellExec,
                        AgentCapability::LongContext,
                    ],
                    available: true,
                }
            }
            async fn health_check(&self) -> bool {
                true
            }
            async fn run(
                &self,
                req: ChatRequest,
                tx: mpsc::Sender<AgentEvent>,
            ) -> anyhow::Result<()> {
                let msg = &req.message;
                if msg.contains("ONLY a JSON array") {
                    let _ = tx.send(AgentEvent::Token { delta: self.plan.to_string() }).await;
                } else if msg.contains("ONLY the candidate number") {
                    // Selector: always pick candidate 1.
                    let _ = tx.send(AgentEvent::Token { delta: "1".into() }).await;
                } else if msg.contains("EDITING THE FILES") {
                    // CODE prompt: actually write a file into the worktree (cwd).
                    if let Some(root) = req.project_root.as_ref() {
                        let model = req.model.as_deref().unwrap_or("unknown");
                        let fname = format!("edit_{}.txt", model.replace([':', '/'], "_"));
                        let _ = std::fs::write(
                            Path::new(root).join(fname),
                            format!("change by {model}\n"),
                        );
                    }
                    let _ = tx
                        .send(AgentEvent::Token {
                            delta: "Created the requested file.".into(),
                        })
                        .await;
                } else {
                    let _ = tx
                        .send(AgentEvent::Token { delta: "ok".into() })
                        .await;
                }
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                Ok(())
            }
        }

        // Plan: one CODE subtask, fanned out, so ≥2 worktrees race.
        let plan = r#"[{"id":"s1","task":"add a file","kind":"code","difficulty":"hard","fan_out":true}]"#;
        let mut r = Registry::new();
        r.register(Arc::new(CodeAdapter { id: "claude-cli", plan }));
        r.register(Arc::new(CodeAdapter { id: "gateway-remote", plan }));
        let reg = Arc::new(RwLock::new(r));
        let store = TracingStore::in_memory();
        let ws = WorktreeStore::new(store.shared_connection());

        let events = Arc::new(parking_lot::Mutex::new(Vec::<UltEvent>::new()));
        let ev = events.clone();
        let cfg = UltimateConfig {
            goal: "add a file to the repo".into(),
            project_root: Some(project.display().to_string()),
            fan_out: 2,
            lead_model: Some("claude-opus-4-8".into()),
        };

        let result = run_ultimate(reg, store, cfg, move |e| ev.lock().push(e))
            .await
            .expect("ultimate code run completes");

        // The subtask edited files (non-empty diff captured) and references a
        // worktree branch the user can review.
        assert_eq!(result.subtasks.len(), 1);
        let st = &result.subtasks[0];
        assert!(st.ok, "winner produced an on-disk change");
        assert!(
            st.output.contains("Branch to review/merge: `cortex/"),
            "result threads the winner branch: {}",
            st.output
        );
        assert!(st.output.contains("```diff"), "diff captured into the result");

        // A SubtaskMerged event announced the SELECTED candidate.
        let evs = events.lock().clone();
        // This adapter returns no valid merged diff for the merge prompt, so the
        // AUTO-MERGE attempt FALLS BACK to selection — the event says so.
        assert!(
            evs.iter().any(|e| matches!(e, UltEvent::SubtaskMerged { id, merged }
                if id == "s1" && merged.contains("selected candidate"))),
            "a selection (fallback) event was emitted"
        );

        // Cleanup: exactly ONE active worktree remains (the kept result's); both
        // the loser AND the torn-down merge worktree are gone — no orphans.
        let active = ws.list_active(Some(&project.display().to_string())).unwrap();
        assert_eq!(active.len(), 1, "only the kept worktree survives: {active:?}");
        assert!(Path::new(&active[0].path).exists(), "kept worktree still on disk");
    }

    /// LIVE/ignored: the git-worktree CODE path AUTO-MERGE happy path. Like the
    /// test above but the merge model returns a REAL unified diff that applies
    /// cleanly to the fresh merge worktree, so the engine keeps the MERGE branch
    /// (not a selected candidate) and tears every other worktree down.
    #[tokio::test]
    #[ignore]
    async fn code_subtask_auto_merges_candidate_diffs_when_diff_applies() {
        use std::path::Path;

        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let project = home.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let git = |args: &[&str]| {
            crate::sys::no_window("git").arg("-C").arg(&project).args(args).output().unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        let _ = git(&["config", "user.name", "test"]);
        let _ = git(&["config", "user.email", "test@local"]);
        std::fs::write(project.join("README.md"), "seed\n").unwrap();
        let _ = git(&["add", "-A"]);
        assert!(git(&["commit", "-q", "-m", "seed"]).status.success());

        // A merged.txt file that does NOT exist at the base, so the merge diff
        // (adding it) applies cleanly against the fresh merge worktree.
        const MERGED_DIFF: &str = "diff --git a/merged.txt b/merged.txt\n\
            new file mode 100644\n\
            index 0000000..9daeafb\n\
            --- /dev/null\n\
            +++ b/merged.txt\n\
            @@ -0,0 +1 @@\n\
            +merged content\n";

        struct MergeAdapter {
            id: &'static str,
            plan: &'static str,
        }
        #[async_trait]
        impl AgentAdapter for MergeAdapter {
            fn descriptor(&self) -> AgentDescriptor {
                AgentDescriptor {
                    id: self.id.to_string(),
                    label: self.id.to_string(),
                    description: String::new(),
                    capabilities: vec![
                        AgentCapability::Chat,
                        AgentCapability::CodeEdit,
                        AgentCapability::ShellExec,
                        AgentCapability::LongContext,
                    ],
                    available: true,
                }
            }
            async fn health_check(&self) -> bool {
                true
            }
            async fn run(
                &self,
                req: ChatRequest,
                tx: mpsc::Sender<AgentEvent>,
            ) -> anyhow::Result<()> {
                let msg = &req.message;
                if msg.contains("ONLY a JSON array") {
                    let _ = tx.send(AgentEvent::Token { delta: self.plan.to_string() }).await;
                } else if msg.contains("Merged unified diff:") {
                    // The merge model: emit a real, applicable unified diff
                    // (fenced, to also exercise sanitize_diff).
                    let fenced = format!("```diff\n{MERGED_DIFF}```");
                    let _ = tx.send(AgentEvent::Token { delta: fenced }).await;
                } else if msg.contains("EDITING THE FILES") {
                    // CODE prompt: each candidate edits a distinct file.
                    if let Some(root) = req.project_root.as_ref() {
                        let model = req.model.as_deref().unwrap_or("unknown");
                        let fname = format!("edit_{}.txt", model.replace([':', '/'], "_"));
                        let _ = std::fs::write(
                            Path::new(root).join(fname),
                            format!("change by {model}\n"),
                        );
                    }
                    let _ = tx
                        .send(AgentEvent::Token { delta: "Created the file.".into() })
                        .await;
                } else {
                    let _ = tx.send(AgentEvent::Token { delta: "ok".into() }).await;
                }
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                Ok(())
            }
        }

        let plan = r#"[{"id":"s1","task":"add a file","kind":"code","difficulty":"hard","fan_out":true}]"#;
        let mut r = Registry::new();
        r.register(Arc::new(MergeAdapter { id: "claude-cli", plan }));
        r.register(Arc::new(MergeAdapter { id: "gateway-remote", plan }));
        let reg = Arc::new(RwLock::new(r));
        let store = TracingStore::in_memory();
        let ws = WorktreeStore::new(store.shared_connection());

        let events = Arc::new(parking_lot::Mutex::new(Vec::<UltEvent>::new()));
        let ev = events.clone();
        let cfg = UltimateConfig {
            goal: "add a file to the repo".into(),
            project_root: Some(project.display().to_string()),
            fan_out: 2,
            lead_model: Some("claude-opus-4-8".into()),
        };

        let result = run_ultimate(reg, store, cfg, move |e| ev.lock().push(e))
            .await
            .expect("ultimate code run completes");

        // The result is the AUTO-MERGE: header + the merged.txt change landed.
        assert_eq!(result.subtasks.len(), 1);
        let st = &result.subtasks[0];
        assert!(st.ok, "merge produced an on-disk change");
        assert!(
            st.output.contains("AUTO-MERGED"),
            "result announces the auto-merge: {}",
            st.output
        );
        assert!(st.output.contains("merged.txt"), "the merged change is present");

        // The merge event announced an auto-merge (not a selection fallback).
        let evs = events.lock().clone();
        assert!(
            evs.iter().any(|e| matches!(e, UltEvent::SubtaskMerged { id, merged }
                if id == "s1" && merged.contains("AUTO-MERGED"))),
            "an auto-merge event was emitted: {evs:?}"
        );

        // Cleanup: exactly ONE active worktree (the merge's); both candidate
        // worktrees were torn down — no orphans.
        let active = ws.list_active(Some(&project.display().to_string())).unwrap();
        assert_eq!(active.len(), 1, "only the merge worktree survives: {active:?}");
        // The surviving worktree actually contains the merged file.
        assert!(
            Path::new(&active[0].path).join("merged.txt").exists(),
            "merge worktree holds the merged change"
        );
    }
}
