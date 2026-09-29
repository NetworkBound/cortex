//! Scheduled agents — "Routines".
//!
//! A Routine is a saved agent task (a name + a prompt) that runs on an interval
//! or at a daily wall-clock time and records its runs. A background scheduler
//! (spawned in `lib.rs` setup, mirroring `gitea_backup::spawn_scheduler`) ticks
//! every 30s, runs any due routine, records the outcome, and emits events.
//! Routines can also be triggered manually with `run_routine_now`.
//!
//! Execution has two paths, chosen by [`choose_dispatch`]: a routine with an
//! `agent_id` (or any routine when no Cortex Gateway is configured) runs
//! through the adapter registry — the same `oneshot` helper `/review` and
//! inline assist use — with `project_root` as the CLI's working directory, and
//! is recorded as an `agent.run` span in the tracing store like a chat turn.
//! Otherwise it goes through the Cortex Gateway as before. A CLI-only user with
//! no local agent installed gets one clear error instead of a connection
//! failure against an empty gateway URL.
//!
//! Every run — manual or scheduled — appends a `RoutineRun` record to
//! `~/.cortex/routine-runs.json` (capped per routine, newest first) and emits
//! `routines:run-recorded` with the full record so the NotificationCenter sees
//! outcomes regardless of which tab is open. Scheduled failures additionally
//! fire an OS desktop notification — the whole point of routines is running
//! while the user looks elsewhere. A run can be reopened as a chat session
//! (`routine_run_as_session`) so its output is a real conversation turn the
//! user can continue.
//!
//! Storage is `~/.cortex/routines.json`. The due-check + upsert + history-cap
//! logic are pure functions, unit-tested without a scheduler or gateway.

use crate::agents::adapter::{AgentCapability, AgentDescriptor, AgentEvent};
use crate::agents::oneshot;
use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;

/// Serializes every read-modify-write of `routines.json` within this process so
/// concurrent mutators (scheduler vs. UI commands) can't lose updates. Never
/// held across an `.await` — only around the short load/modify/save sections.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// Poison-tolerant lock: a panic while holding the guard must not brick every
/// subsequent routines command until app restart. The protected state is
/// re-loaded from disk inside each critical section, so a poisoned guard
/// carries no torn in-memory state worth refusing over.
fn store_guard() -> std::sync::MutexGuard<'static, ()> {
    STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineSpec {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    /// Run cadence in minutes. 0 = manual-only (never auto-fires).
    #[serde(default)]
    pub interval_minutes: u64,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_run_unix_ms: i64,
    #[serde(default)]
    pub last_status: String, // "" | "ok" | "error"
    #[serde(default)]
    pub last_output: String,
    #[serde(default)]
    pub last_error: String,
    /// Registry id of the adapter to run through (`claude-cli`, `codex-cli`,
    /// …; a model slug is accepted too). `None` = the Cortex Gateway when one
    /// is configured, else the first available local agent.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Working directory for the run (a project root). Only CLI adapters use
    /// it; the gateway path ignores it.
    #[serde(default)]
    pub project_root: Option<String>,
    /// Daily wall-clock time `"HH:MM"` (24h, local time). When set it takes
    /// precedence over `interval_minutes`.
    #[serde(default)]
    pub daily_at: Option<String>,
    /// Output-only: unix ms of the next scheduled run (`None` = manual-only or
    /// disabled). Recomputed on every list; whatever is in the file is ignored.
    #[serde(default)]
    pub next_run_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineStore {
    #[serde(default)]
    pub routines: Vec<RoutineSpec>,
}

/// One completed run of a routine — manual or scheduled. Persisted newest-first
/// in `~/.cortex/routine-runs.json`, capped at [`RUNS_PER_ROUTINE_CAP`] per
/// routine. `prompt` is snapshotted at run time (the routine may be edited
/// later) so `routine_run_as_session` can reconstruct a faithful chat turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineRun {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub routine_id: String,
    #[serde(default)]
    pub routine_name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub started_unix_ms: i64,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub status: String, // "ok" | "error"
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub trigger: String, // "manual" | "scheduled"
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineRunLog {
    #[serde(default)]
    pub runs: Vec<RoutineRun>,
}

/// History depth kept per routine. An hourly routine retains ~2 days of runs;
/// the cap is per-routine so one chatty 15-min routine can't evict a daily
/// one's history.
const RUNS_PER_ROUTINE_CAP: usize = 50;

// ----- pure helpers (unit-tested) -----

/// Parse `"HH:MM"` (24h). Lenient on a single-digit hour (`"8:05"`), strict
/// on ranges. `None` for anything else.
fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// Unix ms of `date` at `h:m` wall-clock in `tz`. A time that doesn't exist
/// (spring-forward DST gap) is shifted forward one hour; an ambiguous one
/// (fall-back) takes the earlier instant — so a `02:30` routine still fires
/// once on both switch days.
fn slot_ms<Tz: TimeZone>(tz: &Tz, date: chrono::NaiveDate, h: u32, m: u32) -> Option<i64> {
    let naive = date.and_hms_opt(h, m, 0)?;
    if let Some(dt) = tz.from_local_datetime(&naive).earliest() {
        return Some(dt.timestamp_millis());
    }
    tz.from_local_datetime(&(naive + chrono::Duration::hours(1)))
        .earliest()
        .map(|dt| dt.timestamp_millis())
}

/// When should `r` run next, given the wall clock is `now_ms` in `tz`?
///
/// * disabled → `None`
/// * `daily_at` set (and valid) → the most recent `HH:MM` occurrence that has
///   not been served yet (a missed slot is caught up as soon as the app is
///   running again; a routine that has never run waits for its first slot
///   instead of firing the moment it is created), else the next occurrence
/// * `interval_minutes > 0` → now if never run, else `last_run + interval`
/// * neither → `None` (manual-only)
///
/// Generic over the time zone so tests pin a `FixedOffset` while the app uses
/// `Local`. Nothing here formats or assumes a fixed offset per day, which is
/// what keeps it correct across DST changes.
fn next_run_ms_in<Tz: TimeZone>(tz: &Tz, r: &RoutineSpec, now_ms: i64) -> Option<i64> {
    if !r.enabled {
        return None;
    }
    if let Some((h, m)) = r.daily_at.as_deref().and_then(parse_hhmm) {
        let now = tz.timestamp_millis_opt(now_ms).single()?;
        let today = now.date_naive();
        let today_slot = slot_ms(tz, today, h, m)?;
        let (most_recent, next) = if today_slot <= now_ms {
            (today_slot, slot_ms(tz, today.succ_opt()?, h, m)?)
        } else {
            (slot_ms(tz, today.pred_opt()?, h, m)?, today_slot)
        };
        if r.last_run_unix_ms == 0 || r.last_run_unix_ms >= most_recent {
            return Some(next);
        }
        return Some(most_recent);
    }
    if r.interval_minutes > 0 {
        if r.last_run_unix_ms == 0 {
            return Some(now_ms);
        }
        return Some(r.last_run_unix_ms + (r.interval_minutes as i64) * 60_000);
    }
    None
}

fn next_run_ms(r: &RoutineSpec, now_ms: i64) -> Option<i64> {
    next_run_ms_in(&chrono::Local, r, now_ms)
}

/// Fill `next_run_unix_ms` on every routine for list output.
fn with_next_runs(mut routines: Vec<RoutineSpec>, now_ms: i64) -> Vec<RoutineSpec> {
    for r in &mut routines {
        r.next_run_unix_ms = next_run_ms(r, now_ms);
    }
    routines
}

/// IDs of routines that are due to run at `now_ms`: enabled and scheduled
/// (interval or daily time) with a next-run instant at or before now.
fn due_routines(now_ms: i64, routines: &[RoutineSpec]) -> Vec<String> {
    routines
        .iter()
        .filter(|r| next_run_ms(r, now_ms).is_some_and(|t| t <= now_ms))
        .map(|r| r.id.clone())
        .collect()
}

/// `Some("")`/whitespace → `None`, so the store never carries a blank
/// override that reads as "set" downstream.
fn norm(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Insert or replace a routine by id, preserving run-history fields on update.
fn upsert(mut routines: Vec<RoutineSpec>, mut spec: RoutineSpec) -> Vec<RoutineSpec> {
    spec.agent_id = norm(spec.agent_id);
    spec.project_root = norm(spec.project_root);
    spec.daily_at = norm(spec.daily_at);
    spec.next_run_unix_ms = None;
    if let Some(existing) = routines.iter_mut().find(|r| r.id == spec.id) {
        // keep the run history; only the editable fields change.
        existing.name = spec.name;
        existing.prompt = spec.prompt;
        existing.interval_minutes = spec.interval_minutes;
        existing.enabled = spec.enabled;
        existing.agent_id = spec.agent_id;
        existing.project_root = spec.project_root;
        existing.daily_at = spec.daily_at;
    } else {
        if spec.id.is_empty() {
            spec.id = format!("r-{}", now_ms());
        }
        routines.push(spec);
    }
    routines
}

/// Prepend `run` to the log, evicting the oldest entries of the SAME routine
/// beyond `cap`. Other routines' histories are untouched.
fn push_run(mut runs: Vec<RoutineRun>, run: RoutineRun, cap: usize) -> Vec<RoutineRun> {
    let routine_id = run.routine_id.clone();
    runs.insert(0, run);
    let mut kept = 0usize;
    runs.retain(|r| {
        if r.routine_id != routine_id {
            return true;
        }
        kept += 1;
        kept <= cap
    });
    runs
}

/// Should a run outcome fire an OS desktop notification? Only scheduled
/// failures: manual runs happen in front of the user (the panel toasts), and
/// scheduled successes land quietly in the NotificationCenter inbox — a
/// desktop ping every 15 minutes would train the user to ignore them.
fn should_desktop_notify(status: &str, trigger: &str) -> bool {
    status == "error" && trigger == "scheduled"
}

/// Where a routine's prompt is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dispatch {
    /// The Cortex Gateway (`GatewayClient`, gateway model).
    Gateway,
    /// A registry adapter by id (or a model slug routed like the composer).
    Local(String),
}

/// Registry ids that mean "the gateway" when written into `agent_id`.
const GATEWAY_IDS: &[&str] = &["gateway-remote", "gateway"];

/// The one error a user without any runner sees, everywhere.
const NO_RUNNER_HINT: &str = "This routine has nowhere to run: no Cortex Gateway URL is configured and no local agent CLI (Claude Code, Codex, Gemini, …) was found on PATH. Set a gateway in Settings → Infrastructure, install an agent CLI, or pick a specific agent for this routine.";

/// Decide the execution path for one run. Pure so the matrix is unit-tested:
///
/// | `agent_id`        | gateway configured | local agent found | result          |
/// |-------------------|--------------------|-------------------|-----------------|
/// | explicit local id | any                | any               | Local(id)       |
/// | `gateway-remote`  | yes / no           | any               | Gateway / Err   |
/// | none              | yes                | any               | Gateway         |
/// | none              | no                 | yes               | Local(found)    |
/// | none              | no                 | no                | Err(hint)       |
fn choose_dispatch(
    agent_id: Option<&str>,
    gateway_configured: bool,
    local_agent: Option<&str>,
) -> Result<Dispatch, String> {
    match agent_id.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) if GATEWAY_IDS.contains(&a) => {
            if gateway_configured {
                Ok(Dispatch::Gateway)
            } else {
                Err("This routine is pinned to the Cortex Gateway but no gateway URL is configured (Settings → Infrastructure).".into())
            }
        }
        Some(a) => Ok(Dispatch::Local(a.to_string())),
        None if gateway_configured => Ok(Dispatch::Gateway),
        None => local_agent
            .map(|a| Dispatch::Local(a.to_string()))
            .ok_or_else(|| NO_RUNNER_HINT.to_string()),
    }
}

/// Preferred local agents, in order, for routines that don't name one.
const LOCAL_PREFERENCE: &[&str] = &["claude-cli", "codex-cli", "gemini-cli", "aider-cli"];

/// The best available non-gateway chat adapter, or `None` when there isn't
/// one (nothing installed / nothing reachable). The E2E stub never counts.
fn pick_local_agent(descriptors: &[AgentDescriptor]) -> Option<String> {
    let usable = |d: &&AgentDescriptor| {
        d.available
            && !GATEWAY_IDS.contains(&d.id.as_str())
            && d.id != "e2e-fake"
            && d.capabilities.contains(&AgentCapability::Chat)
    };
    for pref in LOCAL_PREFERENCE {
        if let Some(d) = descriptors.iter().filter(usable).find(|d| d.id == *pref) {
            return Some(d.id.clone());
        }
    }
    let mut rest: Vec<&AgentDescriptor> = descriptors.iter().filter(usable).collect();
    rest.sort_by(|a, b| a.id.cmp(&b.id)); // registry order is a HashMap's
    rest.first().map(|d| d.id.clone())
}

/// E2E-only deterministic stand-in for the LLM call. When the app runs under
/// `CORTEX_E2E=1`, prompts beginning with the magic markers short-circuit
/// `llm_complete` so the probe can exercise the FULL run-record → event →
/// notification → open-as-chat chain offline, with both outcomes, regardless
/// of gateway reachability. Returns `None` for every real prompt; production
/// builds never hit it because the env gate is checked first.
fn e2e_fake_result(prompt: &str) -> Option<Result<String, String>> {
    let p = prompt.trim_start();
    if let Some(rest) = p.strip_prefix("[[e2e:ok]]") {
        return Some(Ok(format!("e2e fake routine output for: {}", rest.trim())));
    }
    if p.starts_with("[[e2e:err]]") {
        return Some(Err("e2e fake routine failure".into()));
    }
    None
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ----- store I/O -----

fn store_path() -> Option<PathBuf> {
    crate::paths::cortex_dir().map(|c| c.join("routines.json"))
}

fn load_store() -> RoutineStore {
    let Some(path) = store_path() else {
        return RoutineStore::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return RoutineStore::default(); // absent → empty store
    };
    match serde_json::from_str(&raw) {
        Ok(store) => store,
        Err(_) => {
            // Present but unparseable: preserve the file (a hand-edit typo or a
            // truncated write) instead of silently overwriting it with {} on the
            // next save. Move it aside so the user can recover.
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            RoutineStore::default()
        }
    }
}

fn save_store(store: &RoutineStore) -> Result<(), String> {
    let path = store_path().ok_or("could not resolve ~/.cortex")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    // Atomic write: a crash/concurrent write can't leave a torn file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write routines.json: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("commit routines.json: {e}"))
}

// Run-history log — same load/save idiom as the routine store (preserve an
// unparseable file as `.bad`, atomic tmp+rename writes). Guarded by the same
// STORE_LOCK: runs are always written in the same critical section that
// updates the routine's last_* fields, so the two files can't disagree.

fn runs_path() -> Option<PathBuf> {
    crate::paths::cortex_dir().map(|c| c.join("routine-runs.json"))
}

fn load_runs() -> RoutineRunLog {
    let Some(path) = runs_path() else {
        return RoutineRunLog::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return RoutineRunLog::default();
    };
    match serde_json::from_str(&raw) {
        Ok(log) => log,
        Err(_) => {
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            RoutineRunLog::default()
        }
    }
}

fn save_runs(log: &RoutineRunLog) -> Result<(), String> {
    let path = runs_path().ok_or("could not resolve ~/.cortex")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(log).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write routine-runs.json: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("commit routine-runs.json: {e}"))
}

// ----- execution -----

async fn llm_complete(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let client = GatewayClient::new(base_url.to_string(), api_key.to_string());
    let req = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: system.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user.into(),
            },
        ],
        stream: true,
        temperature: Some(0.4),
    };
    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
    let stream_fut = async move {
        let _ = client.chat_completion_stream(req, tx).await;
    };
    let collect_fut = async {
        let mut buf = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => buf.push_str(&s),
                StreamItem::Done { .. } => break,
            }
        }
        buf
    };
    let (_, body) = tokio::join!(stream_fut, collect_fut);
    if body.trim().is_empty() {
        Err("the model returned an empty response".into())
    } else {
        Ok(body)
    }
}

fn gateway_cfg(state: &AppState) -> (String, String, String) {
    let cfg = state.config.read();
    (
        cfg.gateway_base_url.clone(),
        AppState::get_gateway_api_key().unwrap_or_default(),
        cfg.gateway_model.clone(),
    )
}

/// Generous ceiling for a local CLI run: agents doing real work in a repo can
/// legitimately take minutes; a hung CLI must still release the scheduler.
const LOCAL_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// The fields of a routine a run needs, snapshotted under the store lock.
struct RunTarget {
    name: String,
    prompt: String,
    agent_id: Option<String>,
    project_root: Option<String>,
}

/// Pick the execution path for `target` from the live app state (gateway URL,
/// registry descriptors). Surfaces [`NO_RUNNER_HINT`] when there is none.
fn decide_dispatch(state: &AppState, target: &RunTarget) -> Result<Dispatch, String> {
    let gateway_configured = !state.config.read().gateway_base_url.trim().is_empty();
    let local = pick_local_agent(&state.registry.read().list_descriptors());
    choose_dispatch(
        target.agent_id.as_deref(),
        gateway_configured,
        local.as_deref(),
    )
}

/// Session id under which a routine's local runs are recorded in the tracing
/// store (one session per routine; each run is its own trace/span).
fn trace_session_id(routine_id: &str) -> String {
    format!("routine-{routine_id}")
}

/// Run `prompt` through a registry adapter with `project_root` as cwd, and
/// record it as `chat.turn` + `agent.run` spans keyed by `run_id` — the same
/// bookkeeping `chat.rs`/`issues.rs` do, so the run shows up in Run Replay
/// and the token/reliability dashboards. Recording is best-effort.
async fn local_complete(
    state: &AppState,
    store: &TracingStore,
    agent: &str,
    target: &RunTarget,
    routine_id: &str,
    run_id: &str,
) -> Result<(String, String), String> {
    let project_root = match target.project_root.as_deref() {
        Some(p) => {
            let pb = PathBuf::from(p);
            if !pb.is_dir() {
                return Err(format!(
                    "project root `{p}` is not a directory — fix the routine's project or clear it"
                ));
            }
            Some(pb)
        }
        None => None,
    };

    // Registry id first; anything else is treated as a model slug and routed
    // exactly like the composer's model picker would route it.
    let registry = state.registry.clone();
    let by_id = registry.read().get(agent).map(|a| (a, agent.to_string()));
    let (adapter, agent_id, model) = match by_id {
        Some((a, id)) => (a, id, None),
        None => {
            let (a, id) = oneshot::resolve_adapter(&registry, Some(agent))?;
            (a, id, Some(agent.to_string()))
        }
    };

    let prompt = format!(
        "You are running the saved Cortex routine \u{201c}{}\u{201d} unattended. Complete the task and report the result concisely.\n\n{}",
        target.name, target.prompt
    );

    let session_id = trace_session_id(routine_id);
    let reason = format!("routine `{}` ({})", target.name, agent);
    let _ = store.record_chat_turn(
        run_id,
        &session_id,
        &prompt,
        std::slice::from_ref(&agent_id),
        Some(reason.as_str()),
    );
    let _ = store.start_agent_run(run_id, run_id, &session_id, &agent_id, model.as_deref());

    let result = match tokio::time::timeout(
        LOCAL_RUN_TIMEOUT,
        oneshot::collect_completion_in(adapter, model, prompt, project_root),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(format!(
            "routine timed out after {} minutes",
            LOCAL_RUN_TIMEOUT.as_secs() / 60
        )),
    };

    let evt = match &result {
        Ok(_) => AgentEvent::Done {
            total_tokens: None,
            run_id: Some(run_id.to_string()),
        },
        Err(e) => AgentEvent::Error { message: e.clone() },
    };
    let _ = store.record_event(run_id, &evt);
    let _ = store.finish_agent_run(run_id);
    result.map(|text| (text, agent_id))
}

/// Run one routine by id, persist the outcome onto its record, and append a
/// [`RoutineRun`] to the history log. Returns the updated spec + the run.
///
/// The store snapshot is NOT held across the LLM call: we read just the
/// name/prompt/agent/project, run the model with no snapshot in hand, then
/// re-load and update only the run-result fields on the still-present routine.
/// That way a concurrent edit or delete during the (multi-second) run isn't
/// clobbered. Both the read and the write sections take `STORE_LOCK`; neither
/// spans the `.await`.
///
/// Takes the app state + trace store directly (no Tauri handle) so the
/// mobile server's `POST /api/v2/routines/:id/run` and the headless
/// `cortex-serve` binary run routines through this exact path.
pub async fn run_and_record(
    state: &AppState,
    store: &TracingStore,
    id: &str,
    trigger: &str,
) -> Result<(RoutineSpec, RoutineRun), String> {
    let target = {
        let _g = store_guard();
        load_store()
            .routines
            .iter()
            .find(|r| r.id == id)
            .map(|r| RunTarget {
                name: r.name.clone(),
                prompt: r.prompt.clone(),
                agent_id: r.agent_id.clone(),
                project_root: r.project_root.clone(),
            })
            .ok_or("routine not found")?
    };

    let started = now_ms();
    let run_id = format!("rr-{}-{}", started, &uuid::Uuid::new_v4().to_string()[..8]);
    let result = match crate::commands::e2e::e2e_enabled()
        .then(|| e2e_fake_result(&target.prompt))
        .flatten()
    {
        Some(fake) => fake,
        None => match decide_dispatch(state, &target) {
            Err(e) => Err(e),
            Ok(Dispatch::Local(agent)) => {
                local_complete(state, store, &agent, &target, id, &run_id)
                    .await
                    .map(|(text, _agent_id)| text)
            }
            Ok(Dispatch::Gateway) => {
                let (base, key, model) = gateway_cfg(state);
                llm_complete(
                    &base,
                    &key,
                    &model,
                    "You are an automation agent running a saved routine. Complete the task concisely and report the result.",
                    &target.prompt,
                )
                .await
            }
        },
    };
    let now = now_ms();

    let mut run = RoutineRun {
        run_id,
        routine_id: id.to_string(),
        routine_name: target.name,
        prompt: target.prompt.chars().take(4000).collect(),
        started_unix_ms: started,
        duration_ms: (now - started).max(0),
        trigger: trigger.to_string(),
        ..Default::default()
    };

    let _g = store_guard();
    let mut routines = load_store();
    let r = routines
        .routines
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or("routine was removed during its run")?;
    r.last_run_unix_ms = now;
    match &result {
        Ok(out) => {
            r.last_status = "ok".into();
            r.last_output = out.chars().take(8000).collect();
            r.last_error = String::new();
            run.status = "ok".into();
            run.output = r.last_output.clone();
        }
        Err(e) => {
            r.last_status = "error".into();
            r.last_error = e.clone();
            run.status = "error".into();
            run.error = e.clone();
        }
    }
    let mut updated = r.clone();
    save_store(&routines)?;
    updated.next_run_unix_ms = next_run_ms(&updated, now);

    // History is best-effort relative to the spec update: a failed log write
    // must not turn a successful run into a command error.
    let mut log = load_runs();
    log.runs = push_run(log.runs, run.clone(), RUNS_PER_ROUTINE_CAP);
    if let Err(e) = save_runs(&log) {
        tracing::warn!("routine run history write failed: {e}");
    }
    Ok((updated, run))
}

/// Shared executor for manual + scheduled runs: run, record, then fan the
/// outcome out — `routines:ran` (legacy panel refresh), `routines:run-recorded`
/// (full record; feeds the NotificationCenter from any tab), and an OS desktop
/// notification when a scheduled run fails (see [`should_desktop_notify`]).
pub async fn execute_routine(
    app: &tauri::AppHandle,
    id: &str,
    trigger: &str,
) -> Result<RoutineSpec, String> {
    let (state, store) = {
        let state = app.state::<AppState>().inner().clone();
        let store = app.state::<TracingStore>().inner().clone();
        (state, store)
    };
    let (spec, run) = run_and_record(&state, &store, id, trigger).await?;
    let _ = app.emit("routines:ran", &id);
    let _ = app.emit("routines:run-recorded", &run);
    notify_run_outcome(&run);
    Ok(spec)
}

/// Desktop + phone-push notifications for a finished run. Split from
/// [`execute_routine`] so a run triggered without a Tauri handle (mobile API,
/// headless) reports failures the same way.
pub fn notify_run_outcome(run: &RoutineRun) {
    if should_desktop_notify(&run.status, &run.trigger) {
        // Best-effort: a missing notification daemon must not fail the run.
        let _ = crate::commands::notify::fire(
            &format!("Routine \u{201c}{}\u{201d} failed", run.routine_name),
            &run.error,
        );
        // Phone push (opt-in, best-effort, off-thread).
        crate::commands::push_notify::notify_routine_failed(
            &run.run_id,
            &run.routine_name,
            &run.error,
        );
    }
}

/// Background scheduler — spawned once at app setup. Ticks every 30s and runs
/// any due routine through the same record/notify path as manual runs.
pub fn spawn_scheduler(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Let first-run IO settle before the first tick.
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let due = due_routines(now_ms(), &load_store().routines);
            if due.is_empty() {
                continue;
            }
            for id in due {
                if let Err(e) = execute_routine(&app, &id, "scheduled").await {
                    tracing::warn!("routine {id} failed: {e}");
                }
            }
        }
    });
}

// ----- Tauri commands -----

/// Every routine, with `next_run_unix_ms` filled in.
#[tauri::command]
pub fn list_routines() -> Result<Vec<RoutineSpec>, String> {
    Ok(with_next_runs(load_store().routines, now_ms()))
}

/// Validate the editable fields before they reach the store. Pure; the
/// messages are what the panel shows next to the form.
fn validate_spec(routine: &RoutineSpec) -> Result<(), String> {
    if routine.name.trim().is_empty() {
        return Err("Give the routine a name.".into());
    }
    if routine.prompt.trim().is_empty() {
        return Err("Give the routine a task prompt.".into());
    }
    if let Some(t) = routine
        .daily_at
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if parse_hhmm(t).is_none() {
            return Err(format!("Daily time `{t}` must be HH:MM (24-hour)."));
        }
    }
    if let Some(p) = routine
        .project_root
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if !Path::new(p).is_dir() {
            return Err(format!("Project folder `{p}` does not exist."));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn save_routine(routine: RoutineSpec) -> Result<Vec<RoutineSpec>, String> {
    validate_spec(&routine)?;
    let _g = store_guard();
    let mut store = load_store();
    store.routines = upsert(store.routines, routine);
    save_store(&store)?;
    Ok(with_next_runs(store.routines, now_ms()))
}

#[tauri::command]
pub fn delete_routine(id: String) -> Result<Vec<RoutineSpec>, String> {
    let _g = store_guard();
    let mut store = load_store();
    store.routines.retain(|r| r.id != id);
    save_store(&store)?;
    // Purge the deleted routine's history too — orphan runs would otherwise
    // accumulate forever and resurface confusingly if the id were ever reused.
    let mut log = load_runs();
    let before = log.runs.len();
    log.runs.retain(|r| r.routine_id != id);
    if log.runs.len() != before {
        if let Err(e) = save_runs(&log) {
            tracing::warn!("routine run history purge failed: {e}");
        }
    }
    Ok(with_next_runs(store.routines, now_ms()))
}

#[tauri::command]
pub fn set_routine_enabled(id: String, enabled: bool) -> Result<Vec<RoutineSpec>, String> {
    let _g = store_guard();
    let mut store = load_store();
    if let Some(r) = store.routines.iter_mut().find(|r| r.id == id) {
        r.enabled = enabled;
    }
    save_store(&store)?;
    Ok(with_next_runs(store.routines, now_ms()))
}

#[tauri::command]
pub async fn run_routine_now(id: String, app: tauri::AppHandle) -> Result<RoutineSpec, String> {
    execute_routine(&app, &id, "manual").await
}

/// Run history, newest first. `routine_id = None` returns runs across all
/// routines (the panel filters per routine; the cap keeps totals small).
#[tauri::command]
pub fn list_routine_runs(
    routine_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<RoutineRun>, String> {
    let limit = limit.unwrap_or(RUNS_PER_ROUTINE_CAP);
    Ok(load_runs()
        .runs
        .into_iter()
        .filter(|r| routine_id.as_deref().is_none_or(|id| r.routine_id == id))
        .take(limit)
        .collect())
}

/// Materialize a recorded run as a real chat session: the snapshotted prompt
/// becomes the user turn and the output (or failure) the assistant turn. The
/// frontend then opens it through the existing `cortex:chat-replay` plumbing,
/// so the user can keep talking — subsequent sends thread into this session.
#[tauri::command]
pub async fn routine_run_as_session(
    run_id: String,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    let run = load_runs()
        .runs
        .into_iter()
        .find(|r| r.run_id == run_id)
        .ok_or("run not found — it may have aged out of the history cap")?;

    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    let assistant_body = if run.status == "ok" {
        run.output.clone()
    } else {
        format!("The routine failed:\n\n```\n{}\n```", run.error)
    };
    let user = StoredMessage {
        id: format!("ru-{}", uuid::Uuid::new_v4()),
        session_id: session_id.clone(),
        ts: run.started_unix_ms,
        role: "user".into(),
        agent_id: None,
        content: format!(
            "Routine \u{201c}{}\u{201d} ({} run):\n\n{}",
            run.routine_name, run.trigger, run.prompt
        ),
        run_id: Some(run.run_id.clone()),
        reasoning: None,
        project_root: None,
    };
    let assistant = StoredMessage {
        id: format!("ra-{}", uuid::Uuid::new_v4()),
        ts: run.started_unix_ms + 1, // keep turn order under ts sorting
        role: "assistant".into(),
        content: assistant_body,
        ..user.clone()
    };
    store.record_message(&user).map_err(|e| e.to_string())?;
    store
        .record_message(&assistant)
        .map_err(|e| e.to_string())?;
    Ok(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, interval: u64, enabled: bool, last: i64) -> RoutineSpec {
        RoutineSpec {
            id: id.into(),
            name: id.into(),
            prompt: "do a thing".into(),
            interval_minutes: interval,
            enabled,
            last_run_unix_ms: last,
            ..Default::default()
        }
    }

    #[test]
    fn never_run_enabled_routine_is_due() {
        let now = 10_000_000;
        let r = vec![spec("a", 60, true, 0)];
        assert_eq!(due_routines(now, &r), vec!["a".to_string()]);
    }

    #[test]
    fn recently_run_routine_is_not_due_until_interval_elapses() {
        let now = 10_000_000;
        // ran 30 min ago, interval 60 min → not due
        let r = vec![spec("a", 60, true, now - 30 * 60_000)];
        assert!(due_routines(now, &r).is_empty());
        // ran 61 min ago → due
        let r2 = vec![spec("a", 60, true, now - 61 * 60_000)];
        assert_eq!(due_routines(now, &r2), vec!["a".to_string()]);
    }

    #[test]
    fn disabled_or_manual_routines_never_fire() {
        let now = 10_000_000;
        let r = vec![spec("disabled", 60, false, 0), spec("manual", 0, true, 0)];
        assert!(due_routines(now, &r).is_empty());
    }

    fn run(routine_id: &str, run_id: &str) -> RoutineRun {
        RoutineRun {
            run_id: run_id.into(),
            routine_id: routine_id.into(),
            routine_name: routine_id.into(),
            status: "ok".into(),
            trigger: "manual".into(),
            ..Default::default()
        }
    }

    #[test]
    fn push_run_prepends_newest_first() {
        let log = push_run(vec![run("a", "r1")], run("a", "r2"), 50);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].run_id, "r2", "newest run is first");
        assert_eq!(log[1].run_id, "r1");
    }

    #[test]
    fn push_run_caps_per_routine_without_evicting_others() {
        let mut log = vec![run("daily", "d1")];
        for i in 0..5 {
            log = push_run(log, run("chatty", &format!("c{i}")), 3);
        }
        let chatty: Vec<_> = log.iter().filter(|r| r.routine_id == "chatty").collect();
        assert_eq!(chatty.len(), 3, "chatty routine capped at 3");
        assert_eq!(chatty[0].run_id, "c4", "newest kept");
        assert_eq!(chatty[2].run_id, "c2", "oldest evicted were c0/c1");
        assert!(
            log.iter().any(|r| r.routine_id == "daily"),
            "other routine's history untouched by the cap"
        );
    }

    #[test]
    fn desktop_notify_only_on_scheduled_failure() {
        assert!(should_desktop_notify("error", "scheduled"));
        assert!(!should_desktop_notify("ok", "scheduled"));
        assert!(!should_desktop_notify("error", "manual"));
        assert!(!should_desktop_notify("ok", "manual"));
    }

    #[test]
    fn e2e_fake_markers_short_circuit_and_real_prompts_pass_through() {
        assert!(
            matches!(e2e_fake_result("[[e2e:ok]] say hi"), Some(Ok(s)) if s.contains("say hi"))
        );
        assert!(matches!(e2e_fake_result("  [[e2e:err]]"), Some(Err(_))));
        assert!(e2e_fake_result("summarize the homelab status").is_none());
        assert!(e2e_fake_result("").is_none());
    }

    #[test]
    fn upsert_adds_then_replaces_preserving_history() {
        let routines = upsert(vec![], spec("", 60, true, 0));
        assert_eq!(routines.len(), 1);
        assert!(routines[0].id.starts_with("r-"), "a fresh id is assigned");

        // give it run history, then edit it
        let mut withhist = routines.clone();
        withhist[0].last_run_unix_ms = 123;
        withhist[0].last_status = "ok".into();
        let mut edit = withhist[0].clone();
        edit.name = "renamed".into();
        edit.interval_minutes = 120;
        let after = upsert(withhist, edit);
        assert_eq!(after.len(), 1, "edit replaces, not appends");
        assert_eq!(after[0].name, "renamed");
        assert_eq!(after[0].interval_minutes, 120);
        assert_eq!(
            after[0].last_run_unix_ms, 123,
            "run history preserved across edit"
        );
        assert_eq!(after[0].last_status, "ok");
    }

    #[test]
    fn upsert_carries_and_normalizes_the_new_fields() {
        let mut fresh = spec("", 0, true, 0);
        fresh.agent_id = Some("  claude-cli ".into());
        fresh.project_root = Some("".into());
        fresh.daily_at = Some("08:00".into());
        fresh.next_run_unix_ms = Some(42); // client noise; never stored
        let routines = upsert(vec![], fresh);
        assert_eq!(routines[0].agent_id.as_deref(), Some("claude-cli"));
        assert_eq!(routines[0].project_root, None, "blank → None");
        assert_eq!(routines[0].daily_at.as_deref(), Some("08:00"));
        assert_eq!(routines[0].next_run_unix_ms, None);

        let mut edit = routines[0].clone();
        edit.agent_id = None;
        edit.daily_at = Some("  ".into());
        let after = upsert(routines, edit);
        assert_eq!(after[0].agent_id, None, "edit can clear the agent");
        assert_eq!(after[0].daily_at, None);
    }

    #[test]
    fn parse_hhmm_accepts_24h_and_rejects_garbage() {
        assert_eq!(parse_hhmm("08:00"), Some((8, 0)));
        assert_eq!(parse_hhmm("8:05"), Some((8, 5)));
        assert_eq!(parse_hhmm(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_hhmm("00:00"), Some((0, 0)));
        for bad in [
            "24:00", "12:60", "12", "12:5", "1200", "ab:cd", "", ":30", "-1:00",
        ] {
            assert_eq!(parse_hhmm(bad), None, "{bad:?}");
        }
    }

    /// A fixed zone so the arithmetic below is deterministic on every CI host
    /// regardless of its local time zone.
    fn tz() -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(2 * 3600).unwrap()
    }

    fn at(tz: &chrono::FixedOffset, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn daily(daily_at: &str, last: i64) -> RoutineSpec {
        RoutineSpec {
            id: "d".into(),
            name: "d".into(),
            prompt: "p".into(),
            enabled: true,
            daily_at: Some(daily_at.into()),
            last_run_unix_ms: last,
            ..Default::default()
        }
    }

    #[test]
    fn daily_never_run_waits_for_its_first_slot() {
        let tz = tz();
        // 09:00, routine at 08:00 → tomorrow 08:00 (no fire-on-create).
        let now = at(&tz, 2026, 3, 10, 9, 0);
        assert_eq!(
            next_run_ms_in(&tz, &daily("08:00", 0), now),
            Some(at(&tz, 2026, 3, 11, 8, 0))
        );
        // 07:30 → today 08:00.
        let now = at(&tz, 2026, 3, 10, 7, 30);
        assert_eq!(
            next_run_ms_in(&tz, &daily("08:00", 0), now),
            Some(at(&tz, 2026, 3, 10, 8, 0))
        );
    }

    #[test]
    fn daily_catches_up_a_missed_slot_then_moves_to_tomorrow() {
        let tz = tz();
        let now = at(&tz, 2026, 3, 10, 9, 0);
        // Last ran yesterday 08:00:05 → today's 08:00 slot is unserved → due now.
        let r = daily("08:00", at(&tz, 2026, 3, 9, 8, 0) + 5_000);
        let next = next_run_ms_in(&tz, &r, now).unwrap();
        assert_eq!(next, at(&tz, 2026, 3, 10, 8, 0));
        assert!(next <= now, "due");
        // Ran today 08:00:03 → tomorrow.
        let r = daily("08:00", at(&tz, 2026, 3, 10, 8, 0) + 3_000);
        assert_eq!(
            next_run_ms_in(&tz, &r, now),
            Some(at(&tz, 2026, 3, 11, 8, 0))
        );
    }

    #[test]
    fn daily_across_midnight_uses_yesterdays_slot() {
        let tz = tz();
        // 00:10 on the 11th, routine at 23:30, last ran two days ago → the
        // most recent slot is the 10th 23:30 → due.
        let now = at(&tz, 2026, 3, 11, 0, 10);
        let r = daily("23:30", at(&tz, 2026, 3, 8, 23, 30));
        assert_eq!(
            next_run_ms_in(&tz, &r, now),
            Some(at(&tz, 2026, 3, 10, 23, 30))
        );
        // Served that slot → tonight 23:30.
        let r = daily("23:30", at(&tz, 2026, 3, 10, 23, 30) + 1_000);
        assert_eq!(
            next_run_ms_in(&tz, &r, now),
            Some(at(&tz, 2026, 3, 11, 23, 30))
        );
        // Never run at 00:10 → tonight 23:30, not "now".
        assert_eq!(
            next_run_ms_in(&tz, &daily("23:30", 0), now),
            Some(at(&tz, 2026, 3, 11, 23, 30))
        );
    }

    #[test]
    fn daily_is_computed_in_the_given_zone_not_utc() {
        let east = chrono::FixedOffset::east_opt(10 * 3600).unwrap();
        let west = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        // Same instant: 2026-03-10T12:00Z.
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 3, 10, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        let r = daily("08:00", 0);
        // 22:00 in +10 → next slot is the 11th 08:00 +10 = 10th 22:00Z.
        assert_eq!(
            next_run_ms_in(&east, &r, now),
            Some(at(&east, 2026, 3, 11, 8, 0))
        );
        // 07:00 in -5 → today's 08:00 -5 = 13:00Z.
        assert_eq!(
            next_run_ms_in(&west, &r, now),
            Some(at(&west, 2026, 3, 10, 8, 0))
        );
        assert_ne!(
            next_run_ms_in(&east, &r, now),
            next_run_ms_in(&west, &r, now)
        );
        // The Local path is just the same function with the host zone; it must
        // not panic and must yield a future or catch-up instant, never None.
        assert!(next_run_ms(&r, now).is_some());
    }

    #[test]
    fn dst_gap_and_overlap_still_yield_one_slot() {
        // Europe/Berlin-like rules aren't available without tzdata, but the
        // arithmetic path is exercised with Local: a 02:30 routine on the
        // host's spring-forward day either exists or is shifted +1h; either
        // way `slot_ms` returns Some.
        let d = chrono::NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        assert!(slot_ms(&chrono::Local, d, 2, 30).is_some());
        assert!(slot_ms(&tz(), d, 2, 30).is_some());
        // FixedOffset never has a gap: the slot is exactly 02:30.
        assert_eq!(
            slot_ms(&tz(), d, 2, 30),
            Some(at(&tz(), 2026, 3, 29, 2, 30))
        );
    }

    #[test]
    fn interval_and_precedence_and_disabled() {
        let tz = tz();
        let now = 10_000_000;
        // interval: never run → now; else last + interval.
        assert_eq!(next_run_ms_in(&tz, &spec("a", 60, true, 0), now), Some(now));
        assert_eq!(
            next_run_ms_in(&tz, &spec("a", 60, true, now - 30 * 60_000), now),
            Some(now + 30 * 60_000)
        );
        // manual-only and disabled → None.
        assert_eq!(next_run_ms_in(&tz, &spec("m", 0, true, 0), now), None);
        assert_eq!(next_run_ms_in(&tz, &spec("off", 60, false, 0), now), None);
        let mut off = daily("08:00", 0);
        off.enabled = false;
        assert_eq!(next_run_ms_in(&tz, &off, now), None);
        // daily_at wins over an interval when both are set …
        let mut both = spec("b", 15, true, 0);
        both.daily_at = Some("08:00".into());
        let n = next_run_ms_in(&tz, &both, now).unwrap();
        assert!(n > now, "not 'now' as the interval rule would say");
        // … unless it is unparsable, then the interval applies.
        both.daily_at = Some("eight".into());
        assert_eq!(next_run_ms_in(&tz, &both, now), Some(now));
    }

    #[test]
    fn due_routines_includes_daily_catch_up_and_with_next_runs_fills_field() {
        let now = now_ms();
        // A daily routine whose last run was 2 days ago is due whatever the
        // wall clock says (the most recent slot is < 24h ago > last run).
        let mut r = daily("12:00", now - 2 * 24 * 3_600_000);
        r.id = "daily".into();
        let due = due_routines(now, &[r.clone(), spec("manual", 0, true, 0)]);
        assert_eq!(due, vec!["daily".to_string()]);
        let listed = with_next_runs(vec![r, spec("manual", 0, true, 0)], now);
        assert!(listed[0].next_run_unix_ms.is_some_and(|t| t <= now));
        assert_eq!(listed[1].next_run_unix_ms, None);
    }

    #[test]
    fn validate_spec_checks_time_and_folder() {
        let mut r = spec("v", 60, true, 0);
        assert!(validate_spec(&r).is_ok());
        r.daily_at = Some("25:00".into());
        assert!(validate_spec(&r).unwrap_err().contains("HH:MM"));
        r.daily_at = Some("07:45".into());
        r.project_root = Some("/definitely/not/a/dir/xyzzy-42".into());
        assert!(validate_spec(&r).unwrap_err().contains("does not exist"));
        r.project_root = Some(std::env::temp_dir().to_string_lossy().to_string());
        assert!(validate_spec(&r).is_ok());
        r.name = " ".into();
        assert!(validate_spec(&r).is_err());
    }

    #[test]
    fn dispatch_matrix() {
        // Explicit local agent always wins.
        assert_eq!(
            choose_dispatch(Some("codex-cli"), true, None),
            Ok(Dispatch::Local("codex-cli".into()))
        );
        assert_eq!(
            choose_dispatch(Some(" claude-cli "), false, None),
            Ok(Dispatch::Local("claude-cli".into()))
        );
        // Pinned to the gateway.
        assert_eq!(
            choose_dispatch(Some("gateway-remote"), true, None),
            Ok(Dispatch::Gateway)
        );
        assert!(choose_dispatch(Some("gateway-remote"), false, Some("claude-cli")).is_err());
        // No agent: gateway if configured, else the local pick, else the hint.
        assert_eq!(
            choose_dispatch(None, true, Some("claude-cli")),
            Ok(Dispatch::Gateway)
        );
        assert_eq!(choose_dispatch(Some(""), true, None), Ok(Dispatch::Gateway));
        assert_eq!(
            choose_dispatch(None, false, Some("claude-cli")),
            Ok(Dispatch::Local("claude-cli".into()))
        );
        let err = choose_dispatch(None, false, None).unwrap_err();
        assert!(err.contains("no Cortex Gateway URL"), "{err}");
        assert!(err.contains("agent CLI"), "{err}");
    }

    fn desc(id: &str, available: bool, chat: bool) -> AgentDescriptor {
        AgentDescriptor {
            id: id.into(),
            label: id.into(),
            description: String::new(),
            capabilities: if chat {
                vec![AgentCapability::Chat]
            } else {
                vec![]
            },
            available,
        }
    }

    #[test]
    fn pick_local_agent_prefers_known_clis_and_skips_gateway_and_stub() {
        assert_eq!(pick_local_agent(&[]), None);
        assert_eq!(
            pick_local_agent(&[
                desc("gateway-remote", true, true),
                desc("e2e-fake", true, true)
            ]),
            None
        );
        assert_eq!(
            pick_local_agent(&[
                desc("ollama", true, true),
                desc("codex-cli", true, true),
                desc("claude-cli", false, true),
            ]),
            Some("codex-cli".into()),
            "claude-cli is preferred but unavailable → codex"
        );
        assert_eq!(
            pick_local_agent(&[desc("zeta", true, true), desc("alpha", true, true)]),
            Some("alpha".into()),
            "no preferred id → deterministic alphabetical"
        );
        assert_eq!(
            pick_local_agent(&[desc("claude-cli", true, false)]),
            None,
            "an adapter without Chat can't run a prompt"
        );
    }
}
