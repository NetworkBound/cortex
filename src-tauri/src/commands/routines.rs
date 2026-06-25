//! Scheduled agents — "Routines".
//!
//! A Routine is a saved agent task (a name + a prompt) that runs on an interval
//! and records its runs. A background scheduler (spawned in `lib.rs` setup,
//! mirroring `gitea_backup::spawn_scheduler`) ticks every 30s, runs any due
//! routine through the Cortex Gateway, records the outcome, and emits events.
//! Routines can also be triggered manually with `run_routine_now`.
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

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;

/// Serializes every read-modify-write of `routines.json` within this process so
/// concurrent mutators (scheduler vs. UI commands) can't lose updates. Never
/// held across an `.await` — only around the short load/modify/save sections.
static STORE_LOCK: Mutex<()> = Mutex::new(());

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

/// IDs of routines that are due to run at `now_ms`: enabled, with a positive
/// interval, that have either never run or whose interval has elapsed.
fn due_routines(now_ms: i64, routines: &[RoutineSpec]) -> Vec<String> {
    routines
        .iter()
        .filter(|r| r.enabled && r.interval_minutes > 0)
        .filter(|r| {
            r.last_run_unix_ms == 0
                || now_ms - r.last_run_unix_ms >= (r.interval_minutes as i64) * 60_000
        })
        .map(|r| r.id.clone())
        .collect()
}

/// Insert or replace a routine by id, preserving run-history fields on update.
fn upsert(mut routines: Vec<RoutineSpec>, mut spec: RoutineSpec) -> Vec<RoutineSpec> {
    if let Some(existing) = routines.iter_mut().find(|r| r.id == spec.id) {
        // keep the run history; only the editable fields change.
        existing.name = spec.name;
        existing.prompt = spec.prompt;
        existing.interval_minutes = spec.interval_minutes;
        existing.enabled = spec.enabled;
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
    dirs::home_dir().map(|h| h.join(".cortex").join("routines.json"))
}

fn load_store() -> RoutineStore {
    let Some(path) = store_path() else { return RoutineStore::default() };
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
    dirs::home_dir().map(|h| h.join(".cortex").join("routine-runs.json"))
}

fn load_runs() -> RoutineRunLog {
    let Some(path) = runs_path() else { return RoutineRunLog::default() };
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

async fn llm_complete(base_url: &str, api_key: &str, model: &str, system: &str, user: &str) -> Result<String, String> {
    let client = GatewayClient::new(base_url.to_string(), api_key.to_string());
    let req = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system.into() },
            ChatMessage { role: "user".into(), content: user.into() },
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

fn gateway_cfg(app: &tauri::AppHandle) -> (String, String, String) {
    let state = app.state::<AppState>();
    let cfg = state.config.read();
    (
        cfg.gateway_base_url.clone(),
        AppState::get_gateway_api_key().unwrap_or_default(),
        cfg.gateway_model.clone(),
    )
}

/// Run one routine by id, persist the outcome onto its record, and append a
/// [`RoutineRun`] to the history log. Returns the updated spec + the run.
///
/// The store snapshot is NOT held across the LLM call: we read just the
/// name/prompt, run the model with no snapshot in hand, then re-load and
/// update only the run-result fields on the still-present routine. That way a
/// concurrent edit or delete during the (multi-second) run isn't clobbered.
/// Both the read and the write sections take `STORE_LOCK`; neither spans the
/// `.await`.
async fn run_and_record(
    id: &str,
    base: &str,
    key: &str,
    model: &str,
    trigger: &str,
) -> Result<(RoutineSpec, RoutineRun), String> {
    let (name, prompt) = {
        let _g = STORE_LOCK.lock().unwrap();
        load_store()
            .routines
            .iter()
            .find(|r| r.id == id)
            .map(|r| (r.name.clone(), r.prompt.clone()))
            .ok_or("routine not found")?
    };

    let started = now_ms();
    let result = match crate::commands::e2e::e2e_enabled()
        .then(|| e2e_fake_result(&prompt))
        .flatten()
    {
        Some(fake) => fake,
        None => {
            llm_complete(
                base,
                key,
                model,
                "You are an automation agent running a saved routine. Complete the task concisely and report the result.",
                &prompt,
            )
            .await
        }
    };
    let now = now_ms();

    let mut run = RoutineRun {
        run_id: format!("rr-{}-{}", now, &uuid::Uuid::new_v4().to_string()[..8]),
        routine_id: id.to_string(),
        routine_name: name,
        prompt: prompt.chars().take(4000).collect(),
        started_unix_ms: started,
        duration_ms: (now - started).max(0),
        trigger: trigger.to_string(),
        ..Default::default()
    };

    let _g = STORE_LOCK.lock().unwrap();
    let mut store = load_store();
    let r = store
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
    let updated = r.clone();
    save_store(&store)?;

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
async fn execute_routine(
    app: &tauri::AppHandle,
    id: &str,
    trigger: &str,
) -> Result<RoutineSpec, String> {
    let (base, key, model) = gateway_cfg(app);
    let (spec, run) = run_and_record(id, &base, &key, &model, trigger).await?;
    let _ = app.emit("routines:ran", &id);
    let _ = app.emit("routines:run-recorded", &run);
    if should_desktop_notify(&run.status, &run.trigger) {
        // Best-effort: a missing notification daemon must not fail the run.
        let _ = crate::commands::notify::fire(
            &format!("Routine \u{201c}{}\u{201d} failed", run.routine_name),
            &run.error,
        );
    }
    Ok(spec)
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

#[tauri::command]
pub fn list_routines() -> Result<Vec<RoutineSpec>, String> {
    Ok(load_store().routines)
}

#[tauri::command]
pub fn save_routine(routine: RoutineSpec) -> Result<Vec<RoutineSpec>, String> {
    if routine.name.trim().is_empty() {
        return Err("Give the routine a name.".into());
    }
    if routine.prompt.trim().is_empty() {
        return Err("Give the routine a task prompt.".into());
    }
    let _g = STORE_LOCK.lock().unwrap();
    let mut store = load_store();
    store.routines = upsert(store.routines, routine);
    save_store(&store)?;
    Ok(store.routines)
}

#[tauri::command]
pub fn delete_routine(id: String) -> Result<Vec<RoutineSpec>, String> {
    let _g = STORE_LOCK.lock().unwrap();
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
    Ok(store.routines)
}

#[tauri::command]
pub fn set_routine_enabled(id: String, enabled: bool) -> Result<Vec<RoutineSpec>, String> {
    let _g = STORE_LOCK.lock().unwrap();
    let mut store = load_store();
    if let Some(r) = store.routines.iter_mut().find(|r| r.id == id) {
        r.enabled = enabled;
    }
    save_store(&store)?;
    Ok(store.routines)
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
    store.record_message(&assistant).map_err(|e| e.to_string())?;
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
        assert!(matches!(e2e_fake_result("[[e2e:ok]] say hi"), Some(Ok(s)) if s.contains("say hi")));
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
        assert_eq!(after[0].last_run_unix_ms, 123, "run history preserved across edit");
        assert_eq!(after[0].last_status, "ok");
    }
}
