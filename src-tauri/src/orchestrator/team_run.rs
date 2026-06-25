//! Team execution — the engine that makes Orchestrator teams actually work.
//!
//! Before this module, `~/.cortex/teams/*.json` was a static demo: workers were
//! minted `idle` and nothing in the app ever dispatched anything to them. This
//! is the real plan→execute loop behind the dashboard's "Assign goal" action:
//!
//! 1. **Plan** — the manager runs ONE one-shot completion through the routed
//!    adapter registry: given the goal and the worker roster, it returns a JSON
//!    array assigning one concrete task per worker. The raw plan is recorded as
//!    a chat session (`Team::plan_session_id`) so the breakdown is inspectable.
//! 2. **Execute** — every worker runs its task concurrently through the same
//!    registry (honoring the role's `model`/`system_prompt` when set). Status
//!    transitions (`idle → working → done|error`) and the assigned task are
//!    persisted via `teams::patch_worker` after every change, so the dashboard
//!    poll shows live progress, and each worker's full transcript is recorded
//!    as a chat session (`Worker::session_id`) for `cortex:chat-replay`.
//!
//! Everything here is Tauri-free (registry + tracing store in, callback out)
//! so the whole engine is exercisable from tests — including the live-Ollama
//! integration test at the bottom. The `run_team` command wraps this with an
//! AppHandle, an in-flight guard and `teams:updated` emissions.

use crate::agents::adapter::AgentCapability;
use crate::agents::{oneshot, Registry};
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use crate::orchestrator::cost_router::{self, Difficulty};
use crate::orchestrator::teams::{self, Team, Worker};
use crate::pricing::{compute_usd, lookup_price};
use parking_lot::RwLock;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

/// Manager planning is a single structured completion — interactive-ish.
const PLAN_TIMEOUT: Duration = Duration::from_secs(120);
/// Workers do the actual work; give them real room (a coding task through a
/// local model or the Claude CLI can legitimately take minutes).
const WORKER_TIMEOUT: Duration = Duration::from_secs(420);
/// The synthesis pass is one structured completion over the (already-produced)
/// worker outputs — sized like the manager's plan, not a worker's open-ended job.
const SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(180);
/// Below this combined worker-output token estimate a synthesis pass isn't worth
/// a strong-model call — the outputs are too thin to meaningfully merge (slice 5
/// gate). ~120 tokens ≈ a few sentences total across all workers.
const SYNTHESIS_MIN_TOKENS: u64 = 120;

/// What kind of work a subtask is. Drives slice-3 capability requirements (a
/// `Code` task needs `CodeEdit`/`ShellExec`; a `Chat` task only `Chat`).
/// Defaults to `Chat` — the conservative, always-satisfiable requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskKind {
    #[default]
    Chat,
    Code,
}

impl TaskKind {
    /// Lowercase wire form, stored verbatim on the `Worker` record.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskKind::Chat => "chat",
            TaskKind::Code => "code",
        }
    }
    /// Tolerant parse — case-insensitive with a few synonyms; unknown → `None`
    /// so a single bad tag falls back to the default instead of failing the plan.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" | "text" | "write" | "writing" | "review" | "analysis" => Some(TaskKind::Chat),
            "code" | "coding" | "edit" | "repo" | "dev" => Some(TaskKind::Code),
            _ => None,
        }
    }
}

/// How demanding a subtask is. Drives slice-3 cheap-vs-strong routing via
/// [`crate::orchestrator::cost_router::Difficulty`]. `Medium` is the honest
/// default for an untagged/unparseable plan — neither the cheapest nor the
/// strongest tier is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskDifficulty {
    Easy,
    #[default]
    Medium,
    Hard,
}

impl TaskDifficulty {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskDifficulty::Easy => "easy",
            TaskDifficulty::Medium => "medium",
            TaskDifficulty::Hard => "hard",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "easy" | "trivial" | "simple" | "low" => Some(TaskDifficulty::Easy),
            "medium" | "moderate" | "normal" | "mid" => Some(TaskDifficulty::Medium),
            "hard" | "difficult" | "complex" | "high" => Some(TaskDifficulty::Hard),
            _ => None,
        }
    }
}

/// One planned unit of work: `worker_id` → what that worker should do, tagged
/// with its `kind` + `difficulty` (slice 2). Nothing dispatches on the tags yet;
/// slice 3 reads them to route a model when the role pins none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub worker_id: String,
    pub task: String,
    pub kind: TaskKind,
    pub difficulty: TaskDifficulty,
}

/// Lenient deserialization target for the manager's JSON plan. The `kind` /
/// `difficulty` tags are read as free strings (not enums) so a single unknown
/// tag can't fail the whole-array parse — they're normalized with defaults in
/// [`parse_assignments`].
#[derive(Debug, Deserialize)]
struct RawAssignment {
    worker_id: String,
    task: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    difficulty: Option<String>,
}

/// Names of teams currently executing — `run_team` refuses a second concurrent
/// run on the same team. `Vec` because `Vec::new()` is const; N is tiny.
static RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Mark a team as running. Returns `false` (and changes nothing) when a run is
/// already in flight for it.
pub fn try_begin(team_id: &str) -> bool {
    let mut g = RUNNING.lock().unwrap();
    if g.iter().any(|id| id == team_id) {
        return false;
    }
    g.push(team_id.to_string());
    true
}

pub fn finish(team_id: &str) {
    RUNNING.lock().unwrap().retain(|id| id != team_id);
}

pub fn is_running(team_id: &str) -> bool {
    RUNNING.lock().unwrap().iter().any(|id| id == team_id)
}

/// Build the manager's planning prompt: goal + roster → strict-JSON tasking.
pub fn build_plan_prompt(team: &Team, goal: &str) -> String {
    let mut roster = String::new();
    for w in &team.workers {
        let desc = crate::agents::roles::get_role(&w.role)
            .and_then(|r| r.description)
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        roster.push_str(&format!("- worker_id \"{}\": role \"{}\"{}\n", w.agent_id, w.role, desc));
    }
    format!(
        "You are \"{manager}\", the manager of the team \"{name}\".\n\
         Team goal: {goal}\n\n\
         Your workers:\n{roster}\n\
         Break the goal into one concrete, self-contained task per worker, suited to \
         each worker's role. Tasks must be directly actionable from their text alone.\n\
         Tag each task with a \"kind\" — \"code\" if it edits or runs a repository, \
         otherwise \"chat\" — and a \"difficulty\" — \"easy\" for routine work, \"hard\" \
         for demanding work.\n\
         Respond with ONLY a JSON array — no prose, no code fences:\n\
         [{{\"worker_id\": \"<id from the roster>\", \"task\": \"<the task>\", \"kind\": \"chat|code\", \"difficulty\": \"easy|hard\"}}]\n\
         Every worker_id from the roster must appear exactly once.",
        manager = team.manager_role,
        name = team.name,
    )
}

/// Build a worker's execution prompt. Prepends the role's `system_prompt` when
/// one is defined so role personas authored under Roles actually apply.
pub fn build_worker_prompt(team: &Team, worker: &Worker, goal: &str, task: &str) -> String {
    let persona = crate::agents::roles::get_role(&worker.role)
        .and_then(|r| r.system_prompt)
        .map(|p| format!("{}\n\n", p.trim()))
        .unwrap_or_default();
    format!(
        "{persona}You are the \"{role}\" worker on team \"{name}\".\n\
         Team goal: {goal}\n\
         Your assigned task: {task}\n\n\
         Complete the task now and reply with your finished work product. Be concrete \
         and complete. If the task genuinely cannot be done from this prompt alone, say \
         exactly what is missing.",
        role = worker.role,
        name = team.name,
    )
}

/// Parse the manager's plan into exactly one assignment per roster worker.
///
/// Tolerant by design (small local models fence, preface, or drop entries):
/// extract the first `[…]` span, parse what's salvageable, keep only known
/// worker ids (first mention wins), then backfill any unassigned worker with
/// the verbatim goal so a sloppy plan degrades to "everyone works the goal"
/// instead of a dead run.
pub fn parse_assignments(raw: &str, team: &Team, goal: &str) -> Vec<Assignment> {
    let parsed: Vec<RawAssignment> = extract_json_array(raw)
        .and_then(|span| serde_json::from_str::<Vec<RawAssignment>>(&span).ok())
        .unwrap_or_default();

    let mut out: Vec<Assignment> = Vec::with_capacity(team.workers.len());
    for w in &team.workers {
        let planned = parsed
            .iter()
            .find(|a| a.worker_id == w.agent_id && !a.task.trim().is_empty());
        let (task, kind, difficulty) = match planned {
            Some(a) => (
                a.task.trim().to_string(),
                a.kind.as_deref().and_then(TaskKind::parse).unwrap_or_default(),
                a.difficulty
                    .as_deref()
                    .and_then(TaskDifficulty::parse)
                    .unwrap_or_default(),
            ),
            // Backfill: an unassigned worker works the verbatim goal at default
            // (chat / medium) tags.
            None => (
                goal.trim().to_string(),
                TaskKind::default(),
                TaskDifficulty::default(),
            ),
        };
        out.push(Assignment {
            worker_id: w.agent_id.clone(),
            task,
            kind,
            difficulty,
        });
    }
    out
}

/// First balanced-bracket `[…]` span in the text, fences and prose ignored.
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

/// Capabilities a subtask of `kind` requires of its model (slice 3). A `Code`
/// task needs to read/produce code (`CodeEdit`); a `Chat` task only `Chat`.
///
/// We deliberately do NOT require `ShellExec` for `Code` here: slice-3 dispatch
/// is still a one-shot chat completion, so a cheap local code-capable model
/// (the Ollama adapter advertises `CodeEdit`) is a valid target. Actually
/// *running* a repo edit through a worktree is slice 4 (Lanes), which carries
/// the stricter `ShellExec` requirement — and the cost router's safety property
/// (chat-only direct adapters never advertise `ShellExec`) protects that path.
fn required_caps(kind: TaskKind) -> Vec<AgentCapability> {
    match kind {
        TaskKind::Chat => vec![AgentCapability::Chat],
        TaskKind::Code => vec![AgentCapability::Chat, AgentCapability::CodeEdit],
    }
}

/// Map a plan's difficulty onto the cost router's cheap-vs-strong axis. `Medium`
/// returns `None` — it carries no clear cost signal (slice 2's honest default),
/// so such workers stay on the team's default route rather than being forced to
/// either price extreme.
fn cost_difficulty(d: TaskDifficulty) -> Option<Difficulty> {
    match d {
        TaskDifficulty::Easy => Some(Difficulty::Easy),
        TaskDifficulty::Hard => Some(Difficulty::Hard),
        TaskDifficulty::Medium => None,
    }
}

/// Price + locality for a concrete model slug chosen *outside* the cost router
/// (a role's static pin, or the team's run-model default route). Local Ollama
/// slugs are free; everything else is priced through the shared table after
/// alias resolution (so `opus` prices as `claude-opus-4-8`). Returns
/// `(input_per_million, output_per_million, is_local)`.
fn price_for_slug(slug: &str) -> (f64, f64, bool) {
    let s = slug.trim();
    if s.is_empty() {
        return (0.0, 0.0, false);
    }
    if s.starts_with("ollama:") || s.starts_with("ollama/") {
        return (0.0, 0.0, true);
    }
    let resolved = crate::orchestrator::aliases::resolve_model(s);
    let (inp, outp) = lookup_price(&resolved);
    (inp, outp, false)
}

/// Rough token estimate for cost *projection*: ~4 chars per token, the standard
/// BPE-average heuristic. The adapters surface no real per-token usage
/// (`CompletionOutcome` carries only text), so this is an honest estimate, not a
/// billed figure — hence "projected".
fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// The model a worker will run on plus the pricing needed to project its cost.
struct Routed {
    /// Dispatch slug (`None` → the adapter's default route, unpriced).
    model: Option<String>,
    input_price: f64,
    output_price: f64,
    #[allow(dead_code)] // recorded for clarity / future budget surfacing (slice 6)
    local: bool,
}

/// Decide which model a worker runs on and how to price it (slice 3).
///
/// Precedence:
/// 1. **A role's static model pin always wins** — honored verbatim. This is the
///    invariant the cost router must never override (regression-protected).
/// 2. Otherwise, when the plan's difficulty gives a clear cost signal
///    (`easy`/`hard`), route through [`cost_router::pick_model_for`] for the
///    cheapest / strongest *capable* model the live registry can reach.
/// 3. Failing both (a `medium`/untagged task, or the router finds nothing
///    capable), fall back to the team's run-model default route.
fn route_worker(
    registry: &RwLock<Registry>,
    role_model: Option<&str>,
    kind: TaskKind,
    difficulty: TaskDifficulty,
    run_model: Option<&str>,
    local_tags: &[String],
) -> Routed {
    // 1. Static role pin — never overridden by cost routing.
    if let Some(m) = role_model.map(str::trim).filter(|s| !s.is_empty()) {
        let (input_price, output_price, local) = price_for_slug(m);
        return Routed { model: Some(m.to_string()), input_price, output_price, local };
    }
    // 2. Cost-aware route on a clear difficulty signal.
    if let Some(diff) = cost_difficulty(difficulty) {
        let caps = required_caps(kind);
        let pick = cost_router::pick_model_for(diff, &caps, &registry.read(), local_tags);
        if let Some(p) = pick {
            return Routed {
                model: Some(p.model),
                input_price: p.input_price_per_million_usd,
                output_price: p.output_price_per_million_usd,
                local: p.local,
            };
        }
    }
    // 3. Default route — the team's run-model (may be None → adapter default).
    match run_model.map(str::trim).filter(|s| !s.is_empty()) {
        Some(m) => {
            let (input_price, output_price, local) = price_for_slug(m);
            Routed { model: Some(m.to_string()), input_price, output_price, local }
        }
        None => Routed { model: None, input_price: 0.0, output_price: 0.0, local: false },
    }
}

/// Outcome of dispatching a repo-editing subtask as a worktree lane (slice 4).
pub struct LaneDispatch {
    /// The `lane_runs` row id — linked onto the [`Worker`] so the dashboard can
    /// deep-link the run into the Lanes tab (no dead-end output).
    pub lane_run_id: String,
    /// Final lane status: `done | error | stopped | interrupted` (or a timeout
    /// note). Mapped to the worker's `done`/`error` status by the engine.
    pub status: String,
    /// Humanized outcome recorded as the worker's transcript body (the lane
    /// branch + its final detail line).
    pub outcome: String,
    /// The lane provider the work ran on — recorded as the worker's
    /// `effective_model` so the routing decision stays visible.
    pub provider: String,
}

/// Bridge from the Tauri-free team engine to the (Tauri-coupled) Lanes
/// machinery (slice 4). A `Code`-tagged subtask is dispatched through this
/// instead of a one-shot chat completion, so the worker actually *edits the
/// repository* in a gateway worktree.
///
/// This is the path that respects the ShellExec-only contract: a worktree lane
/// runs on the gateway (which owns real tool execution), whereas the chat-only direct
/// adapters can never reach it. `execute_team` is handed `None` when no repo is
/// bound to the run, in which case code workers stay on the chat path and
/// describe their change as text rather than failing.
#[async_trait::async_trait]
pub trait LaneDispatcher: Send + Sync {
    /// Start a worktree lane for `task` (with `goal` as run context) and follow
    /// it to a terminal state. `Err` only when the lane could not be started at
    /// all (no gateway, bad repo) — a lane that starts and then fails comes back
    /// `Ok` with `status == "error"`.
    async fn dispatch(&self, goal: &str, task: &str) -> Result<LaneDispatch, String>;
}

/// What one worker produced — the raw material the slice-5 synthesis pass merges
/// into a single result + verification note. Carries the run-accounting fields
/// (`ok`/`projected_usd`) the engine already tallied per worker, plus the
/// `role`/`status`/`output` the synthesizer needs to attribute each piece.
struct WorkerSummary {
    ok: bool,
    projected_usd: f64,
    role: String,
    status: &'static str,
    /// The worker's finished output (chat path) or the lane outcome body (slice
    /// 4) — the latter references the lane branch so the synthesizer can speak to
    /// the diff even though the engine itself stays Tauri-free.
    output: String,
}

/// Run one `Code`-tagged worker through a worktree [`LaneDispatcher`] (slice 4):
/// dispatch the lane, link its id onto the worker, follow it to terminal, and
/// record the outcome as the worker's transcript + status. Returns a
/// [`WorkerSummary`] like the chat path; lanes run on the gateway so we don't project
/// a per-token cost here (`0.0`, excluded from the run total).
#[allow(clippy::too_many_arguments)]
async fn run_lane_worker(
    dispatcher: &dyn LaneDispatcher,
    store: &TracingStore,
    team: &Team,
    worker: &Worker,
    goal: &str,
    task: &str,
    team_id: &str,
    run_id: &str,
    notify: &(dyn Fn() + Send + Sync),
) -> WorkerSummary {
    let title = format!(
        "Team \u{201c}{}\u{201d} — {} worker lane.\n\nTask: {}",
        team.name, worker.role, task
    );
    match dispatcher.dispatch(goal, task).await {
        Ok(d) => {
            // Link the lane id immediately so the dashboard can open the run in
            // the Lanes tab even if it's still settling.
            let _ = teams::set_worker_lane(team_id, &worker.agent_id, &d.lane_run_id);
            let ok = d.status == "done";
            let status = if ok { "done" } else { "error" };
            let body = format!(
                "{}\n\nDispatched as repo lane `{}` — final status: {}. Open it in the \
                 Lanes tab to review the diff and merge the winner.",
                d.outcome, d.lane_run_id, d.status
            );
            let session = record_transcript(store, &title, task, &body, run_id).ok();
            // effective_model = the lane provider; projected cost is $0 here
            // (lane spend is tracked by the gateway, not the per-token estimator).
            let _ = teams::patch_worker(
                team_id,
                &worker.agent_id,
                status,
                None,
                session,
                Some(d.provider),
                Some(0.0),
            );
            notify();
            WorkerSummary {
                ok,
                projected_usd: 0.0,
                role: worker.role.clone(),
                status,
                output: body,
            }
        }
        Err(e) => {
            let body = format!("The repo lane could not be started: {e}");
            let session = record_transcript(store, &title, task, &body, run_id).ok();
            let _ =
                teams::patch_worker(team_id, &worker.agent_id, "error", None, session, None, Some(0.0));
            notify();
            WorkerSummary {
                ok: false,
                projected_usd: 0.0,
                role: worker.role.clone(),
                status: "error",
                output: body,
            }
        }
    }
}

/// One bounded completion through the registry — `agents::oneshot` (the same
/// primitive the eval harness runs on) plus alias resolution and a wall-clock
/// timeout so a wedged adapter can't pin a worker in `working` forever.
async fn run_completion(
    registry: &Arc<RwLock<Registry>>,
    model: Option<&str>,
    prompt: String,
    timeout: Duration,
) -> Result<String, String> {
    let resolved = model
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::orchestrator::aliases::resolve_model);
    // Resilient: retry transient blips + fall through the configured fallback
    // chain so one flaky upstream can't strand a worker in `working`.
    tokio::time::timeout(
        timeout,
        oneshot::complete_resilient(registry, resolved, prompt),
    )
    .await
    .map_err(|_| format!("timed out after {}s", timeout.as_secs()))?
    .map(|o| o.text.trim().to_string())
}

/// Record a (prompt, outcome) pair as a fresh chat session and return its id —
/// the same materialize-as-session shape `routine_run_as_session` uses, so the
/// dashboard can hand the run to `cortex:chat-replay` and the user can keep
/// talking in that session.
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
        id: format!("tw-{}", uuid::Uuid::new_v4()),
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
        id: format!("ta-{}", uuid::Uuid::new_v4()),
        ts: ts + 1, // keep turn order under ts sorting
        role: "assistant".into(),
        content: outcome.to_string(),
        ..user.clone()
    };
    store.record_message(&user)?;
    store.record_message(&assistant)?;
    Ok(session_id)
}

/// Execute one full team run: plan with the manager, fan the tasks out to the
/// workers concurrently, persist every transition. `notify` fires after each
/// persisted change (the command wrapper emits `teams:updated` from it).
/// Errors out early only when the team is gone or the manager can't even be
/// attempted; per-worker failures land as worker `error` states instead.
pub async fn execute_team(
    registry: Arc<RwLock<Registry>>,
    store: TracingStore,
    team_id: String,
    goal: String,
    model: Option<String>,
    lane_dispatcher: Option<Arc<dyn LaneDispatcher>>,
    notify: impl Fn() + Send + Sync,
) -> Result<(), String> {
    let team = teams::get_team(&team_id).ok_or_else(|| format!("team '{team_id}' not found"))?;
    let run_id = format!(
        "{}:{}",
        team_id,
        team.last_run_unix_ms.unwrap_or_default()
    );

    // ── Phase 1: the manager plans ────────────────────────────────────────
    let plan_prompt = build_plan_prompt(&team, &goal);
    let plan_raw =
        run_completion(&registry, model.as_deref(), plan_prompt.clone(), PLAN_TIMEOUT).await;

    let assignments = match &plan_raw {
        Ok(raw) => parse_assignments(raw, &team, &goal),
        Err(e) => {
            // Manager unreachable → the workers won't fare better. Record the
            // failure honestly and stop instead of fanning out N more failures.
            let outcome = format!("The manager could not produce a plan: {e}");
            let plan_session =
                record_transcript(&store, &plan_title(&team), &plan_prompt, &outcome, &run_id)
                    .ok();
            let _ = teams::set_run_status(&team_id, "error", plan_session);
            notify();
            return Err(outcome);
        }
    };

    let plan_session = record_transcript(
        &store,
        &plan_title(&team),
        &plan_prompt,
        plan_raw.as_deref().unwrap_or_default(),
        &run_id,
    )
    .ok();
    teams::set_run_status(&team_id, "running", plan_session).map_err(|e| e.to_string())?;
    for a in &assignments {
        let _ =
            teams::patch_worker(&team_id, &a.worker_id, "working", Some(a.task.clone()), None, None, None);
        // Persist the plan's kind/difficulty tags so cost-aware dispatch below
        // (and the dashboard) can read the manager's classification back.
        let _ = teams::set_worker_tags(&team_id, &a.worker_id, a.kind.as_str(), a.difficulty.as_str());
    }
    notify();

    // ── Phase 2: workers execute concurrently ────────────────────────────
    let notify = &notify;
    let team_ref = &team;
    let registry_ref = &registry;
    let store_ref = &store;
    let goal_ref = goal.as_str();
    let run_id_ref = run_id.as_str();
    let team_id_ref = team_id.as_str();
    let run_model = model.as_deref();

    // Live local Ollama tags the cost router can route easy/cheap work to.
    // Slugs are prefixed `ollama:` to match the composer's picker form. Fetched
    // once (best-effort, empty on no server) so every worker shares the same
    // candidate set without N redundant `/api/tags` round-trips.
    let local_tags: Vec<String> =
        crate::agents::ollama::fetch_tags_at(crate::agents::ollama::LOCAL_OLLAMA)
            .await
            .into_iter()
            .map(|t| format!("ollama:{t}"))
            .collect();
    let local_tags_ref = &local_tags;
    let lane_dispatcher_ref = lane_dispatcher.as_deref();

    let worker_runs = assignments.iter().filter_map(|a| {
        let worker = team.workers.iter().find(|w| w.agent_id == a.worker_id)?;
        Some(async move {
            // Slice 4: a code-tagged subtask with a bound repo *edits* it in a
            // gateway worktree lane instead of producing chat. Falls through to
            // the chat path when no dispatcher is wired (no repo bound).
            if a.kind == TaskKind::Code {
                if let Some(dispatcher) = lane_dispatcher_ref {
                    return run_lane_worker(
                        dispatcher, store_ref, team_ref, worker, goal_ref, &a.task, team_id_ref,
                        run_id_ref, notify,
                    )
                    .await;
                }
            }
            // Cost-aware routing (slice 3): a role pin wins; else route easy/hard
            // on price; else the team's run-model. See [`route_worker`].
            let role_model = crate::agents::roles::get_role(&worker.role).and_then(|r| r.model);
            let routed = route_worker(
                registry_ref,
                role_model.as_deref(),
                a.kind,
                a.difficulty,
                run_model,
                local_tags_ref,
            );
            let prompt = build_worker_prompt(team_ref, worker, goal_ref, &a.task);
            let result =
                run_completion(registry_ref, routed.model.as_deref(), prompt.clone(), WORKER_TIMEOUT)
                    .await;
            let (status, outcome): (&'static str, String) = match &result {
                Ok(out) => ("done", out.clone()),
                Err(e) => ("error", format!("The worker run failed: {e}")),
            };
            // Project the run's cost: prompt tokens were always sent; completion
            // tokens count only when the model actually produced output (an
            // error's text is our synthetic message, not the model's, so it's
            // excluded). Free local models project $0.
            let completion = if result.is_ok() { outcome.as_str() } else { "" };
            let projected_usd = compute_usd(
                estimate_tokens(&prompt),
                estimate_tokens(completion),
                (routed.input_price, routed.output_price),
            );
            let title = format!(
                "Team \u{201c}{}\u{201d} — {} worker run.\n\nTask: {}",
                team_ref.name, worker.role, a.task
            );
            let session = record_transcript(store_ref, &title, &prompt, &outcome, run_id_ref).ok();
            // One write records status + transcript + the routing/cost decision,
            // keeping the concurrent fan-out to a single save per worker.
            let _ = teams::patch_worker(
                team_id_ref,
                &a.worker_id,
                status,
                None,
                session,
                routed.model.clone(),
                Some(projected_usd),
            );
            notify();
            WorkerSummary {
                ok: result.is_ok(),
                projected_usd,
                role: worker.role.clone(),
                status,
                output: outcome,
            }
        })
    });

    let outcomes = futures::future::join_all(worker_runs).await;
    let all_ok = !outcomes.is_empty() && outcomes.iter().all(|s| s.ok);
    // Tally the run's total estimated spend onto the team record. Done here,
    // after the join, so this read-modify-write can't race the workers.
    let mut total_usd: f64 = outcomes.iter().map(|s| s.projected_usd).sum();

    // ── Phase 3: synthesis + verification (slice 5) ───────────────────────
    // Merge every worker's output into one coherent result + a verification note
    // so a multi-worker run isn't N disconnected transcripts the user must
    // stitch together by hand. Gated off for a single-subtask or trivially-thin
    // run (nothing to merge). Best-effort: a synthesis failure never fails the
    // run — the per-worker results already stand on their own.
    if should_synthesize(&outcomes) {
        let synth_prompt = build_synthesis_prompt(team_ref, goal_ref, &outcomes);
        let routed = route_synthesizer(registry_ref, run_model, local_tags_ref);
        match run_completion(
            registry_ref,
            routed.model.as_deref(),
            synth_prompt.clone(),
            SYNTHESIS_TIMEOUT,
        )
        .await
        {
            Ok(merged) if !merged.trim().is_empty() => {
                let title = synthesis_title(team_ref);
                if let Ok(sid) =
                    record_transcript(store_ref, &title, &synth_prompt, &merged, run_id_ref)
                {
                    let _ = teams::set_synthesis_session(team_id_ref, &sid);
                }
                // The synthesizer's own call costs tokens too — fold it into the
                // run total so the cost badge stays honest (free local → $0).
                total_usd += compute_usd(
                    estimate_tokens(&synth_prompt),
                    estimate_tokens(&merged),
                    (routed.input_price, routed.output_price),
                );
                notify();
            }
            Ok(_) => {
                tracing::warn!("team {team_id_ref} synthesis returned empty — skipping");
            }
            Err(e) => {
                tracing::warn!("team {team_id_ref} synthesis failed: {e}");
            }
        }
    }

    let _ = teams::set_spent_usd(team_id_ref, total_usd);
    teams::set_run_status(&team_id, if all_ok { "done" } else { "error" }, None)
        .map_err(|e| e.to_string())?;
    notify();
    Ok(())
}

/// Whether a run earns a synthesis pass (slice 5 gate). Skipped when there's only
/// one subtask (nothing to merge) or the workers' combined output is too thin to
/// be worth a strong-model call. Errored workers still count — a verification
/// note over a partial/failed run is exactly when the merge is most useful.
fn should_synthesize(outcomes: &[WorkerSummary]) -> bool {
    if outcomes.len() < 2 {
        return false;
    }
    let combined: u64 = outcomes.iter().map(|s| estimate_tokens(&s.output)).sum();
    combined >= SYNTHESIS_MIN_TOKENS
}

/// Build the manager's synthesis + verification prompt over every worker's
/// finished output. Asks for ONE merged result followed by an explicit
/// verification section so the pass produces both halves the slice calls for.
fn build_synthesis_prompt(team: &Team, goal: &str, outcomes: &[WorkerSummary]) -> String {
    let mut sections = String::new();
    for s in outcomes {
        sections.push_str(&format!(
            "--- {} [{}] ---\n{}\n\n",
            s.role,
            s.status,
            s.output.trim()
        ));
    }
    format!(
        "You are \u{201c}{manager}\u{201d}, the manager of team \u{201c}{name}\u{201d}. Your \
         workers have finished their subtasks toward this goal.\n\n\
         Team goal: {goal}\n\n\
         Each worker's output (with its final status):\n\n{sections}\
         Produce ONE coherent merged result that accomplishes the goal by integrating the \
         workers' outputs — resolve overlaps, keep the strongest of each, and reconcile any \
         contradictions. Then add a section headed \"## Verification\": state whether the \
         pieces are mutually consistent and whether the goal is fully met, call out any \
         worker that failed or left a gap, and list concrete follow-ups if work remains. Be \
         specific and do not claim work the team did not actually do.",
        manager = team.manager_role,
        name = team.name,
    )
}

fn synthesis_title(team: &Team) -> String {
    format!(
        "Team \u{201c}{}\u{201d} — synthesis & verification ({}).",
        team.name, team.manager_role
    )
}

/// Route the synthesis pass (slice 5). Synthesis is a manager-level reviewer
/// task, so it runs on the team's **run-model** — the model the user chose to
/// drive this run, the same brain that planned it. Only when the run pinned no
/// model do we escalate to the cost router's *strongest* capable pick (a `Hard`
/// chat task), falling back to the adapter default if even that finds nothing.
///
/// Keeping a pinned cheap run on its chosen model (rather than always grabbing
/// the priciest adapter) avoids a surprise provider switch mid-run, and keeps
/// the e2e/offline paths deterministic (they always pin a run-model).
fn route_synthesizer(
    registry: &RwLock<Registry>,
    run_model: Option<&str>,
    local_tags: &[String],
) -> Routed {
    if let Some(m) = run_model.map(str::trim).filter(|s| !s.is_empty()) {
        let (input_price, output_price, local) = price_for_slug(m);
        return Routed { model: Some(m.to_string()), input_price, output_price, local };
    }
    // Unpinned run → escalate to the strongest capable chat model.
    let caps = required_caps(TaskKind::Chat);
    if let Some(p) = cost_router::pick_model_for(Difficulty::Hard, &caps, &registry.read(), local_tags)
    {
        return Routed {
            model: Some(p.model),
            input_price: p.input_price_per_million_usd,
            output_price: p.output_price_per_million_usd,
            local: p.local,
        };
    }
    Routed { model: None, input_price: 0.0, output_price: 0.0, local: false }
}

fn plan_title(team: &Team) -> String {
    format!(
        "Team \u{201c}{}\u{201d} — manager plan ({}).",
        team.name, team.manager_role
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::teams::Worker;

    fn team_with(workers: &[(&str, &str)]) -> Team {
        Team {
            id: "team-test0001".into(),
            name: "checkout-refactor".into(),
            manager_role: "system-architect".into(),
            workers: workers
                .iter()
                .map(|(id, role)| Worker {
                    role: role.to_string(),
                    agent_id: id.to_string(),
                    current_task: None,
                    status: "idle".into(),
                    started_unix_ms: 0,
                    last_event_unix_ms: 0,
                    message_count: 0,
                    session_id: None,
                    task_kind: None,
                    task_difficulty: None,
                    effective_model: None,
                    projected_usd: None,
                    lane_run_id: None,
                })
                .collect(),
            created_unix_ms: 0,
            goal: None,
            run_status: None,
            last_run_unix_ms: None,
            plan_session_id: None,
            spent_usd: None,
            synthesis_session_id: None,
            budget_usd: None,
        }
    }

    #[test]
    fn plan_prompt_lists_roster_and_demands_json() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        let p = build_plan_prompt(&t, "ship dark mode");
        assert!(p.contains("ship dark mode"));
        assert!(p.contains("wkr-a") && p.contains("wkr-b"));
        assert!(p.contains("ONLY a JSON array"));
    }

    #[test]
    fn parses_clean_json_plan() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        let raw = r#"[{"worker_id":"wkr-a","task":"write the CSS"},{"worker_id":"wkr-b","task":"add a snapshot test"}]"#;
        let got = parse_assignments(raw, &t, "goal");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].task, "write the CSS");
        assert_eq!(got[1].task, "add a snapshot test");
    }

    #[test]
    fn parses_fenced_and_prefaced_plan() {
        let t = team_with(&[("wkr-a", "coder")]);
        let raw = "Sure! Here is the plan:\n```json\n[{\"worker_id\": \"wkr-a\", \"task\": \"do the thing\"}]\n```\nLet me know!";
        let got = parse_assignments(raw, &t, "goal");
        assert_eq!(got[0].task, "do the thing");
    }

    #[test]
    fn garbage_plan_falls_back_to_goal_for_everyone() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        for raw in ["no json here at all", "[not even valid", ""] {
            let got = parse_assignments(raw, &t, "ship dark mode");
            assert_eq!(got.len(), 2, "raw={raw:?}");
            assert!(got.iter().all(|a| a.task == "ship dark mode"));
        }
    }

    #[test]
    fn unknown_ids_dropped_and_missing_workers_backfilled() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        // Plan invents a worker and forgets wkr-b.
        let raw = r#"[{"worker_id":"wkr-ghost","task":"haunt"},{"worker_id":"wkr-a","task":"real task"}]"#;
        let got = parse_assignments(raw, &t, "the goal");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].task, "real task");
        assert_eq!(got[1].worker_id, "wkr-b");
        assert_eq!(got[1].task, "the goal");
    }

    #[test]
    fn extract_handles_brackets_inside_strings() {
        let raw = r#"text [{"worker_id":"a","task":"use arr[0] and [1]"}] trailing"#;
        let span = extract_json_array(raw).unwrap();
        assert!(serde_json::from_str::<Vec<RawAssignment>>(&span).is_ok());
    }

    #[test]
    fn plan_prompt_requests_kind_and_difficulty() {
        let t = team_with(&[("wkr-a", "coder")]);
        let p = build_plan_prompt(&t, "ship dark mode");
        assert!(p.contains("\"kind\""));
        assert!(p.contains("\"difficulty\""));
        assert!(p.contains("chat|code") && p.contains("easy|hard"));
    }

    #[test]
    fn parses_tagged_plan_into_kind_and_difficulty() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        let raw = r#"[
          {"worker_id":"wkr-a","task":"edit the parser","kind":"code","difficulty":"hard"},
          {"worker_id":"wkr-b","task":"summarize findings","kind":"chat","difficulty":"easy"}
        ]"#;
        let got = parse_assignments(raw, &t, "goal");
        assert_eq!(got[0].kind, TaskKind::Code);
        assert_eq!(got[0].difficulty, TaskDifficulty::Hard);
        assert_eq!(got[1].kind, TaskKind::Chat);
        assert_eq!(got[1].difficulty, TaskDifficulty::Easy);
    }

    #[test]
    fn untagged_and_invalid_tags_default_to_chat_medium() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        // wkr-a: legacy untagged entry. wkr-b: present but garbage tag values.
        let raw = r#"[
          {"worker_id":"wkr-a","task":"do a"},
          {"worker_id":"wkr-b","task":"do b","kind":"wizardry","difficulty":"impossible"}
        ]"#;
        let got = parse_assignments(raw, &t, "goal");
        for a in &got {
            assert_eq!(a.kind, TaskKind::Chat, "{a:?}");
            assert_eq!(a.difficulty, TaskDifficulty::Medium, "{a:?}");
        }
    }

    #[test]
    fn backfilled_worker_gets_default_tags() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "tester")]);
        // Plan only mentions wkr-a; wkr-b is backfilled with the goal.
        let raw = r#"[{"worker_id":"wkr-a","task":"real","kind":"code","difficulty":"hard"}]"#;
        let got = parse_assignments(raw, &t, "the goal");
        assert_eq!(got[1].worker_id, "wkr-b");
        assert_eq!(got[1].task, "the goal");
        assert_eq!(got[1].kind, TaskKind::Chat);
        assert_eq!(got[1].difficulty, TaskDifficulty::Medium);
    }

    #[test]
    fn tag_parse_is_case_insensitive_with_synonyms() {
        assert_eq!(TaskKind::parse("CODE"), Some(TaskKind::Code));
        assert_eq!(TaskKind::parse(" Coding "), Some(TaskKind::Code));
        assert_eq!(TaskKind::parse("review"), Some(TaskKind::Chat));
        assert_eq!(TaskKind::parse("nonsense"), None);
        assert_eq!(TaskDifficulty::parse("HARD"), Some(TaskDifficulty::Hard));
        assert_eq!(TaskDifficulty::parse("trivial"), Some(TaskDifficulty::Easy));
        assert_eq!(TaskDifficulty::parse("moderate"), Some(TaskDifficulty::Medium));
        assert_eq!(TaskDifficulty::parse("???"), None);
        // as_str round-trips back to the canonical wire form.
        assert_eq!(TaskKind::Code.as_str(), "code");
        assert_eq!(TaskDifficulty::Medium.as_str(), "medium");
    }

    #[test]
    fn running_guard_blocks_double_start() {
        assert!(try_begin("team-guard"));
        assert!(!try_begin("team-guard"));
        assert!(is_running("team-guard"));
        finish("team-guard");
        assert!(!is_running("team-guard"));
        finish("team-guard"); // idempotent
    }

    #[test]
    fn worker_prompt_carries_goal_task_and_role() {
        let t = team_with(&[("wkr-a", "coder")]);
        let p = build_worker_prompt(&t, &t.workers[0], "ship dark mode", "write the CSS");
        assert!(p.contains("\"coder\" worker"));
        assert!(p.contains("ship dark mode"));
        assert!(p.contains("write the CSS"));
    }

    // ── Slice 3: cost-aware routing (deterministic, no live model) ──────────

    use crate::agents::adapter::{
        AgentAdapter, AgentDescriptor, AgentEvent, ChatRequest,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// Minimal adapter stub — a fixed descriptor; `route_worker` only ever reads
    /// `descriptor()`, never `run`.
    struct StubAdapter {
        id: &'static str,
        caps: Vec<AgentCapability>,
    }

    #[async_trait]
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.id.to_string(),
                label: self.id.to_string(),
                description: String::new(),
                capabilities: self.caps.clone(),
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(
            &self,
            _req: ChatRequest,
            _tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn registry_with(adapters: Vec<StubAdapter>) -> RwLock<Registry> {
        let mut r = Registry::new();
        for a in adapters {
            r.register(Arc::new(a));
        }
        RwLock::new(r)
    }

    fn claude_cli_stub() -> StubAdapter {
        StubAdapter {
            id: "claude-cli",
            caps: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::ShellExec,
                AgentCapability::LongContext,
            ],
        }
    }

    fn ollama_stub() -> StubAdapter {
        StubAdapter {
            id: "ollama",
            caps: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::LongContext,
            ],
        }
    }

    #[test]
    fn cost_helpers_map_tags_and_caps() {
        assert_eq!(cost_difficulty(TaskDifficulty::Easy), Some(Difficulty::Easy));
        assert_eq!(cost_difficulty(TaskDifficulty::Hard), Some(Difficulty::Hard));
        // Medium has no clear cost signal → no cost route (stays on default).
        assert_eq!(cost_difficulty(TaskDifficulty::Medium), None);
        // Code requires code-editing; chat only chat. Neither demands ShellExec
        // at this slice (one-shot chat dispatch; Lanes carry that in slice 4).
        assert_eq!(required_caps(TaskKind::Chat), vec![AgentCapability::Chat]);
        assert!(required_caps(TaskKind::Code).contains(&AgentCapability::CodeEdit));
        assert!(!required_caps(TaskKind::Code).contains(&AgentCapability::ShellExec));
    }

    #[test]
    fn price_for_slug_frees_local_and_prices_cloud() {
        assert_eq!(price_for_slug("ollama:llama3.2:1b"), (0.0, 0.0, true));
        assert_eq!(price_for_slug("ollama/llama3.2"), (0.0, 0.0, true));
        // Cloud slug priced via the shared table (Opus 15/75).
        let (inp, outp, local) = price_for_slug("claude-opus-4-8");
        assert_eq!((inp, outp), (15.00, 75.00));
        assert!(!local);
        // An alias resolves before pricing (so `opus` ≈ Opus, not the default).
        let (ai, ao, _) = price_for_slug("opus");
        assert_eq!((ai, ao), (15.00, 75.00));
    }

    #[test]
    fn estimate_tokens_is_roughly_chars_over_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2); // rounds up
    }

    #[test]
    fn static_role_pin_always_wins_over_cost_router() {
        // Even an EASY task with a free local model available must honor a role's
        // explicit pin — the core regression the router must never override.
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_worker(
            &reg,
            Some("claude-opus-4-8"),
            TaskKind::Chat,
            TaskDifficulty::Easy,
            Some("ollama:llama3.2:1b"),
            &["ollama:llama3.2:1b".to_string()],
        );
        assert_eq!(routed.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!((routed.input_price, routed.output_price), (15.00, 75.00));
        assert!(!routed.local);
    }

    #[test]
    fn easy_unpinned_task_routes_to_the_free_local_model() {
        // No role pin + easy → cheapest capable model = the free local Ollama tag.
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_worker(
            &reg,
            None,
            TaskKind::Chat,
            TaskDifficulty::Easy,
            Some("claude-opus-4-8"), // run-model default would be expensive…
            &["ollama:llama3.2:1b".to_string()],
        );
        // …but the cost router overrides it with the free local model.
        assert_eq!(routed.model.as_deref(), Some("ollama:llama3.2:1b"));
        assert!(routed.local);
        assert_eq!((routed.input_price, routed.output_price), (0.0, 0.0));
    }

    #[test]
    fn hard_unpinned_task_routes_to_the_strongest_cloud_model() {
        // No role pin + hard → strongest capable = the Opus served by claude-cli,
        // never the free local model.
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_worker(
            &reg,
            None,
            TaskKind::Chat,
            TaskDifficulty::Hard,
            None,
            &["ollama:llama3.2:1b".to_string()],
        );
        assert!(!routed.local, "hard work must not fall to a free local model");
        assert_eq!(routed.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!((routed.input_price, routed.output_price), (15.00, 75.00));
    }

    #[test]
    fn medium_unpinned_task_falls_back_to_the_run_model() {
        // Medium carries no cost signal → the cost router is not consulted; the
        // worker stays on the team's run-model default route.
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_worker(
            &reg,
            None,
            TaskKind::Chat,
            TaskDifficulty::Medium,
            Some("ollama:llama3.2:1b"),
            &["ollama:llama3.2:1b".to_string()],
        );
        assert_eq!(routed.model.as_deref(), Some("ollama:llama3.2:1b"));
        assert!(routed.local);
        // With no run-model either, it degrades to the adapter default (unpriced).
        let bare = route_worker(&reg, None, TaskKind::Chat, TaskDifficulty::Medium, None, &[]);
        assert!(bare.model.is_none());
        assert_eq!((bare.input_price, bare.output_price), (0.0, 0.0));
    }

    // ── Slice 4: code subtasks route through a worktree Lane ────────────────

    /// Adapter that answers the manager's planning prompt with a JSON plan
    /// tagging the (real, prompt-extracted) worker `code`/`hard`, so the lane
    /// path is reached without a live model. Any non-planning prompt echoes.
    struct PlanAdapter;

    #[async_trait]
    impl AgentAdapter for PlanAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: "plan-stub".into(),
                label: "plan-stub".into(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
            let needle = "worker_id \"";
            let body = if let Some(i) = req.message.find(needle) {
                let rest = &req.message[i + needle.len()..];
                let id = &rest[..rest.find('"').unwrap_or(0)];
                format!(
                    "[{{\"worker_id\":\"{id}\",\"task\":\"edit the parser\",\"kind\":\"code\",\"difficulty\":\"hard\"}}]"
                )
            } else {
                "echo".into()
            };
            let _ = tx.send(AgentEvent::Token { delta: body }).await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            Ok(())
        }
    }

    /// Deterministic [`LaneDispatcher`] — records what it was asked to dispatch
    /// and returns a canned terminal lane, no Tauri/gateway needed.
    struct FakeDispatcher {
        run_id: &'static str,
        status: &'static str,
        provider: &'static str,
    }

    #[async_trait]
    impl LaneDispatcher for FakeDispatcher {
        async fn dispatch(&self, goal: &str, task: &str) -> Result<LaneDispatch, String> {
            assert!(!goal.is_empty() && !task.is_empty(), "lane gets goal + task");
            Ok(LaneDispatch {
                lane_run_id: self.run_id.into(),
                status: self.status.into(),
                outcome: format!("lane handled: {task}"),
                provider: self.provider.into(),
            })
        }
    }

    /// In-process end-to-end: a code-tagged worker is dispatched through the
    /// lane (not a chat completion), its `lane_run_id` is linked, and the lane's
    /// terminal status drives the worker's status + recorded `effective_model`.
    /// `#[ignore]`d like the live test because it sets the process-global `HOME`
    /// (run with `-- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn code_worker_routes_through_the_lane_dispatcher() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let team = teams::create_team("lane-test", "manager", &["coder".into()]).unwrap();
        teams::begin_run(&team.id, "edit the repo").unwrap();

        let mut registry = Registry::new();
        registry.register(Arc::new(PlanAdapter));
        let registry = Arc::new(RwLock::new(registry));
        let store = TracingStore::open_at(tmp.path().join("trace.sqlite")).unwrap();

        let dispatcher: Arc<dyn LaneDispatcher> = Arc::new(FakeDispatcher {
            run_id: "lane-fake-001",
            status: "done",
            provider: "e2e-fake",
        });

        execute_team(
            registry,
            store.clone(),
            team.id.clone(),
            "edit the repo".into(),
            None, // no run-model — the plan adapter is the only route
            Some(dispatcher),
            || {},
        )
        .await
        .expect("team run completes");

        let done = teams::get_team(&team.id).unwrap();
        assert_eq!(done.run_status.as_deref(), Some("done"));
        let w = &done.workers[0];
        assert_eq!(w.task_kind.as_deref(), Some("code"), "manager tagged it code");
        assert_eq!(w.status, "done", "lane done → worker done");
        assert_eq!(w.lane_run_id.as_deref(), Some("lane-fake-001"), "lane linked");
        assert_eq!(w.effective_model.as_deref(), Some("e2e-fake"), "provider recorded");
        // The worker transcript references the lane (no dead-end output).
        let sid = w.session_id.as_deref().expect("transcript recorded");
        let msgs = store.load_session_messages(sid).unwrap();
        assert!(msgs[1].content.contains("lane-fake-001"));
    }

    /// A failed lane (terminal `error`) drives the worker to `error` while still
    /// linking the lane so the user can inspect it.
    #[tokio::test]
    #[ignore]
    async fn failed_lane_marks_worker_error_but_keeps_link() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let team = teams::create_team("lane-err", "manager", &["coder".into()]).unwrap();
        teams::begin_run(&team.id, "edit the repo").unwrap();
        let mut registry = Registry::new();
        registry.register(Arc::new(PlanAdapter));
        let registry = Arc::new(RwLock::new(registry));
        let store = TracingStore::open_at(tmp.path().join("trace.sqlite")).unwrap();
        let dispatcher: Arc<dyn LaneDispatcher> = Arc::new(FakeDispatcher {
            run_id: "lane-fake-err",
            status: "error",
            provider: "e2e-fake",
        });

        execute_team(
            registry,
            store,
            team.id.clone(),
            "edit the repo".into(),
            None,
            Some(dispatcher),
            || {},
        )
        .await
        .expect("team run completes (worker errors are not fatal)");

        let done = teams::get_team(&team.id).unwrap();
        assert_eq!(done.run_status.as_deref(), Some("error"));
        let w = &done.workers[0];
        assert_eq!(w.status, "error");
        assert_eq!(w.lane_run_id.as_deref(), Some("lane-fake-err"));
    }

    #[test]
    fn projected_cost_matches_price_times_estimated_tokens() {
        // A 2000-char prompt + 4000-char output at Opus pricing.
        let prompt = "x".repeat(2000); // ~500 tok
        let output = "y".repeat(4000); // ~1000 tok
        let usd = compute_usd(
            estimate_tokens(&prompt),
            estimate_tokens(&output),
            (15.00, 75.00),
        );
        // 500 * 15/1e6 + 1000 * 75/1e6 = 0.0075 + 0.075 = 0.0825
        assert!((usd - 0.0825).abs() < 1e-9, "got {usd}");
    }

    // ── Slice 5: synthesis + verification pass ──────────────────────────────

    fn summary(role: &str, status: &'static str, output: &str) -> WorkerSummary {
        WorkerSummary {
            ok: status == "done",
            projected_usd: 0.0,
            role: role.into(),
            status,
            output: output.into(),
        }
    }

    #[test]
    fn synthesis_gate_skips_single_subtask() {
        // One worker → nothing to merge, even with a long output.
        let one = vec![summary("coder", "done", &"x".repeat(2000))];
        assert!(!should_synthesize(&one));
        // Zero workers (defensive) → no synthesis.
        assert!(!should_synthesize(&[]));
    }

    #[test]
    fn synthesis_gate_skips_thin_output_but_fires_on_substantial() {
        // Two workers but only a few characters each → below the token gate.
        let thin = vec![summary("a", "done", "ok"), summary("b", "done", "done")];
        assert!(!should_synthesize(&thin), "trivial output should skip");
        // Two workers with real output → over the gate, synthesis fires.
        let rich = vec![
            summary("a", "done", &"alpha ".repeat(60)),
            summary("b", "done", &"beta ".repeat(60)),
        ];
        assert!(should_synthesize(&rich));
        // Errored workers still count — a verification note over a failure is
        // exactly when the merge earns its keep.
        let mixed = vec![
            summary("a", "error", &"the worker run failed: timeout ".repeat(10)),
            summary("b", "done", &"here is the finished analysis ".repeat(10)),
        ];
        assert!(should_synthesize(&mixed));
    }

    #[test]
    fn synthesis_prompt_carries_goal_roles_statuses_and_demands_verification() {
        let t = team_with(&[("wkr-a", "coder"), ("wkr-b", "reviewer")]);
        let outcomes = vec![
            summary("coder", "done", "wrote the parser"),
            summary("reviewer", "error", "the worker run failed: timeout"),
        ];
        let p = build_synthesis_prompt(&t, "ship the parser", &outcomes);
        assert!(p.contains("ship the parser"), "goal present");
        assert!(p.contains("coder") && p.contains("reviewer"), "roles present");
        assert!(p.contains("[done]") && p.contains("[error]"), "statuses present");
        assert!(p.contains("wrote the parser"), "worker output inlined");
        assert!(p.contains("## Verification"), "asks for a verification section");
        assert!(p.contains("merged result") || p.contains("merged"), "asks for a merge");
    }

    #[test]
    fn synthesizer_runs_on_the_pinned_run_model() {
        // A pinned run-model is honored verbatim — synthesis is a manager task
        // on the brain the user chose, no surprise provider switch.
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_synthesizer(&reg, Some("claude-opus-4-8"), &[]);
        assert_eq!(routed.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!((routed.input_price, routed.output_price), (15.00, 75.00));
    }

    #[test]
    fn synthesizer_escalates_to_strongest_when_run_model_unpinned() {
        // No run-model → escalate to the strongest capable cloud model…
        let reg = registry_with(vec![claude_cli_stub(), ollama_stub()]);
        let routed = route_synthesizer(&reg, None, &["ollama:llama3.2:1b".to_string()]);
        assert_eq!(routed.model.as_deref(), Some("claude-opus-4-8"));
        assert!(
            !routed.local,
            "with a cloud model present, synthesis must not fall to a free local model"
        );
        // …with only a local model available it's the strongest thing there is,
        // so synthesis legitimately runs on it (better than no synthesis).
        let local_only = registry_with(vec![ollama_stub()]);
        let only_local =
            route_synthesizer(&local_only, None, &["ollama:llama3.2:1b".to_string()]);
        assert_eq!(only_local.model.as_deref(), Some("ollama:llama3.2:1b"));
        // Nothing available at all → the adapter's own default route (None).
        let bare = route_synthesizer(&local_only, None, &[]);
        assert!(bare.model.is_none(), "no candidates → adapter default route");
    }

    /// Adapter that plays all three roles in a team run deterministically:
    /// answers the manager's planning prompt with a 2-worker chat plan, returns a
    /// substantial answer for each worker, and a merged result + verification for
    /// the synthesis prompt. Lets us prove slice 5 end-to-end with no live model.
    struct SynthAdapter;

    #[async_trait]
    impl AgentAdapter for SynthAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: "synth-stub".into(),
                label: "synth-stub".into(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
            let msg = &req.message;
            let body = if msg.contains("Respond with ONLY a JSON array") {
                // Plan: one chat/medium task per roster worker.
                let needle = "worker_id \"";
                let ids: Vec<String> = msg
                    .match_indices(needle)
                    .filter_map(|(i, _)| {
                        let rest = &msg[i + needle.len()..];
                        rest.find('"').map(|e| rest[..e].to_string())
                    })
                    .collect();
                let entries: Vec<String> = ids
                    .iter()
                    .map(|id| {
                        format!("{{\"worker_id\":\"{id}\",\"task\":\"work {id}\",\"kind\":\"chat\",\"difficulty\":\"medium\"}}")
                    })
                    .collect();
                format!("[{}]", entries.join(","))
            } else if msg.contains("## Verification") {
                // Synthesis prompt → a merged result + verification note.
                "MERGED: both worker outputs are combined into one result.\n\n\
                 ## Verification\nThe pieces are consistent and the goal is met."
                    .into()
            } else {
                // Worker prompt → a substantial answer (over the synthesis gate
                // once two of them combine).
                "Here is a thorough, finished work product for the assigned task, \
                 with enough substance to be worth merging into the team result. "
                    .repeat(2)
            };
            let _ = tx.send(AgentEvent::Token { delta: body }).await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            Ok(())
        }
    }

    /// In-process end-to-end (slice 5): a 2-worker run records a synthesis
    /// session with non-empty merged content + a verification note, stamped onto
    /// `Team.synthesis_session_id`. `#[ignore]`d like the other HOME-touching
    /// engine tests (run with `-- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn two_worker_run_records_a_synthesis_session() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let team =
            teams::create_team("synth-test", "manager", &["coder".into(), "reviewer".into()])
                .unwrap();
        teams::begin_run(&team.id, "build and review the feature").unwrap();

        let mut registry = Registry::new();
        registry.register(Arc::new(SynthAdapter));
        let registry = Arc::new(RwLock::new(registry));
        let store = TracingStore::open_at(tmp.path().join("trace.sqlite")).unwrap();

        execute_team(
            registry,
            store.clone(),
            team.id.clone(),
            "build and review the feature".into(),
            Some("synth-stub".into()), // exact-id route → the stub for plan+workers+synthesis
            None,
            || {},
        )
        .await
        .expect("team run completes");

        let done = teams::get_team(&team.id).unwrap();
        assert_eq!(done.run_status.as_deref(), Some("done"));
        assert_eq!(done.workers.len(), 2);
        for w in &done.workers {
            assert_eq!(w.status, "done", "worker {} not done", w.role);
        }
        let sid = done
            .synthesis_session_id
            .as_deref()
            .expect("synthesis session recorded");
        let msgs = store.load_session_messages(sid).unwrap();
        assert_eq!(msgs.len(), 2, "synthesis is user+assistant");
        let merged = &msgs[1].content;
        assert!(!merged.trim().is_empty(), "synthesis content is empty");
        assert!(merged.contains("Verification"), "verification note present");
    }

    /// A single-worker run must NOT record a synthesis session (the gate).
    #[tokio::test]
    #[ignore]
    async fn single_worker_run_skips_synthesis() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let team = teams::create_team("solo-test", "manager", &["coder".into()]).unwrap();
        teams::begin_run(&team.id, "do the one thing").unwrap();
        let mut registry = Registry::new();
        registry.register(Arc::new(SynthAdapter));
        let registry = Arc::new(RwLock::new(registry));
        let store = TracingStore::open_at(tmp.path().join("trace.sqlite")).unwrap();

        execute_team(
            registry,
            store,
            team.id.clone(),
            "do the one thing".into(),
            Some("synth-stub".into()),
            None,
            || {},
        )
        .await
        .expect("team run completes");

        let done = teams::get_team(&team.id).unwrap();
        assert_eq!(done.run_status.as_deref(), Some("done"));
        assert!(
            done.synthesis_session_id.is_none(),
            "single-worker run must skip synthesis"
        );
    }

    /// Live end-to-end run against a local Ollama (`ollama serve` + llama3.2:1b
    /// pulled). Ignored by default; run with:
    ///   `cargo test --lib orchestrator::team_run::tests::team_run_live_ollama -- --ignored --nocapture`
    /// Proves the REAL engine — manager plan, JSON parse/fallback, concurrent
    /// worker one-shots, status persistence, transcript recording — against a
    /// real model.
    #[tokio::test]
    #[ignore]
    async fn team_run_live_ollama() {
        use crate::agents::ollama::OllamaAgent;

        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let team = teams::create_team(
            "live-test",
            "system-architect",
            &["coder".into(), "reviewer".into()],
        )
        .unwrap();
        teams::begin_run(&team.id, "Explain in a detailed paragraph why Rust prevents data races at compile time, then write a thorough critical review of that explanation.").unwrap();

        let mut registry = Registry::new();
        registry.register(Arc::new(OllamaAgent::new(
            "http://127.0.0.1:11434".into(),
            "llama3.2:1b".into(),
        )));
        let registry = Arc::new(RwLock::new(registry));
        let store = TracingStore::open_at(tmp.path().join("trace.sqlite")).unwrap();

        execute_team(
            registry,
            store.clone(),
            team.id.clone(),
            "Explain in a detailed paragraph why Rust prevents data races at compile time, then write a thorough critical review of that explanation.".into(),
            Some("ollama:llama3.2:1b".into()),
            None, // no repo bound → code workers (if any) stay on the chat path
            || {},
        )
        .await
        .expect("live team run should complete");

        let done = teams::get_team(&team.id).unwrap();
        eprintln!("LIVE TEAM AFTER RUN: {done:#?}");
        assert_eq!(done.run_status.as_deref(), Some("done"));
        assert!(done.plan_session_id.is_some(), "manager plan transcript missing");
        // Slice 3: the run's total spend is recorded. Only Ollama is registered,
        // so every worker — whether routed by the cost router (easy/hard) or via
        // the run-model default (medium) — lands on a free local model, so the
        // whole run projects $0. `Some(0.0)` is the populated, correct value.
        assert_eq!(done.spent_usd, Some(0.0), "all-local run should record $0");
        // Slice 5: a 2-worker run with real output earns a synthesis pass,
        // recorded as its own session with non-empty merged content. It runs on
        // the (local, free) run-model, so the all-local $0 total above holds.
        let synth = done
            .synthesis_session_id
            .as_deref()
            .expect("2-worker run should record a synthesis session");
        let smsgs = store.load_session_messages(synth).unwrap();
        assert_eq!(smsgs.len(), 2, "synthesis is a user+assistant pair");
        assert!(
            !smsgs[1].content.trim().is_empty(),
            "synthesis produced empty content"
        );
        for w in &done.workers {
            assert_eq!(w.status, "done", "worker {} not done", w.role);
            assert!(w.current_task.is_some(), "worker {} has no task", w.role);
            // Slice 2: every dispatched worker is tagged kind+difficulty.
            assert!(w.task_kind.is_some(), "worker {} missing kind tag", w.role);
            assert!(
                w.task_difficulty.is_some(),
                "worker {} missing difficulty tag",
                w.role
            );
            // Slice 3: the chosen model is recorded and (only Ollama available)
            // is a local model; its projected cost is $0.
            let model = w
                .effective_model
                .as_deref()
                .unwrap_or_else(|| panic!("worker {} missing effective_model", w.role));
            assert!(
                model.starts_with("ollama"),
                "worker {} routed to non-local model {model}",
                w.role
            );
            assert_eq!(w.projected_usd, Some(0.0), "local worker {} should project $0", w.role);
            let sid = w.session_id.as_deref().expect("worker transcript missing");
            let msgs = store.load_session_messages(sid).unwrap();
            assert_eq!(msgs.len(), 2, "transcript should be user+assistant");
            assert!(!msgs[1].content.trim().is_empty(), "empty worker output");
        }
    }
}
