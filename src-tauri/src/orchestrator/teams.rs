//! Multi-agent team state — persistent records for the orchestrator dashboard.
//!
//! A "team" is a named coordination unit: one Manager (Orchestrator) coordinating
//! N specialist Workers (coder / tester / reviewer / etc). Each team is persisted
//! at `~/.cortex/teams/<team_id>.json` so the dashboard survives relaunches and
//! a backend-side scheduler (future work) can update worker status by writing
//! to the same files.
//!
//! On-disk schema:
//! ```json
//! {
//!   "id": "team-3f9c1a",
//!   "name": "checkout-refactor",
//!   "manager_role": "system-architect",
//!   "workers": [
//!     {
//!       "role": "coder",
//!       "agent_id": "wkr-7b2e",
//!       "current_task": "rewrite cart adapter",
//!       "status": "working",
//!       "started_unix_ms": 1735000000000,
//!       "last_event_unix_ms": 1735000900000,
//!       "message_count": 12
//!     }
//!   ],
//!   "created_unix_ms": 1735000000000
//! }
//! ```
//!
//! Status values: `idle | working | blocked | done | error`. Anything outside
//! that set is rejected at write-time so the UI can render pills without a
//! runtime guard.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// One specialist worker in a team. `agent_id` is opaque to this module — it's
/// minted at `create_team` time so the UI can address workers individually and
/// later look them up regardless of role-name collisions ("coder" can appear
/// twice in the same team).
///
/// Not `Eq` — `projected_usd` is an `f64` (slice 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    pub role: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    pub status: String,
    pub started_unix_ms: u64,
    pub last_event_unix_ms: u64,
    #[serde(default)]
    pub message_count: u64,
    /// Chat session holding this worker's latest run transcript. Set by the
    /// team runner once the worker finishes (success or failure) so the
    /// dashboard can open the run through `cortex:chat-replay`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Subtask classification persisted from the manager's plan (orchestration
    /// slice 2): `"chat"|"code"`. `None` until a run tags the worker. Consumed by
    /// cost-aware dispatch (slice 3) to route a model when the role pins none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    /// Difficulty tag from the manager's plan: `"easy"|"medium"|"hard"`. `None`
    /// until a run tags the worker. Paired with [`Worker::task_kind`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_difficulty: Option<String>,
    /// The concrete model slug this worker was dispatched on for its latest run
    /// (orchestration slice 3): either the role's static pin or the cost
    /// router's pick. `None` until a run dispatches the worker. Surfaced on the
    /// dashboard so the routing decision is visible, not buried in the logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    /// Estimated USD spent on this worker's latest run (slice 3): a token-count
    /// heuristic × the chosen model's per-million price (`0.0` for free local
    /// models). Summed into [`Team::spent_usd`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projected_usd: Option<f64>,
    /// The `lane_runs` id this worker's repo-editing subtask was dispatched to
    /// (orchestration slice 4): a `Code`-tagged worker with a bound repo runs
    /// its work in a gateway worktree instead of a one-shot chat. `None` for chat
    /// workers (and code workers with no bound repo). Lets the dashboard deep-link
    /// the run into the Lanes tab so the worker's output isn't a dead end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_run_id: Option<String>,
}

/// A persisted team record. `created_unix_ms` lets the dashboard sort by
/// recency without inspecting the file mtime.
///
/// Not `Eq` — `spent_usd` is an `f64` (slice 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub manager_role: String,
    #[serde(default)]
    pub workers: Vec<Worker>,
    pub created_unix_ms: u64,
    /// The goal handed to the manager on the most recent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Lifecycle of the most recent run: `planning | running | done | error`.
    /// `None` means the team has never been run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    /// When the most recent run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_unix_ms: Option<u64>,
    /// Chat session holding the manager's planning transcript for the most
    /// recent run (the goal→assignments breakdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_session_id: Option<String>,
    /// Total estimated USD across the most recent run's workers (slice 3) — the
    /// sum of every [`Worker::projected_usd`]. `None` until a run completes;
    /// `Some(0.0)` is a legitimate value (an all-local run is free, but still
    /// tracked). Surfaced as a cost badge on the dashboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_usd: Option<f64>,
    /// Chat session holding the manager's synthesis + verification pass over all
    /// the workers' transcripts (orchestration slice 5). A strong-tier merge of
    /// the per-worker outputs into one coherent result plus a verification note,
    /// recorded once after the fan-out joins. `None` until a multi-worker run
    /// with enough content produces one (single-worker / trivial runs skip it).
    /// Surfaced as a "Synthesis" deep-link on the dashboard so the merged result
    /// isn't a dead-end output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_session_id: Option<String>,
    /// Optional per-team spend ceiling in USD (orchestration slice 6). A soft
    /// budget the dashboard compares the run's projected [`Team::spent_usd`]
    /// against — never enforced (a run is never blocked or killed); crossing it
    /// just paints a soft over-budget warning. `None` = no budget set. Unlike
    /// the per-run cost fields this is a team-level setting, so [`begin_run`]
    /// deliberately preserves it across runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
}

/// The five statuses the dashboard knows how to render. Keep in lock-step with
/// the CSS pill classes in `global.css` (`.orch-pill-<status>`).
pub const VALID_STATUSES: &[&str] = &["idle", "working", "blocked", "done", "error"];

/// Team-level run lifecycle states (see `Team::run_status`).
pub const VALID_RUN_STATUSES: &[&str] = &["planning", "running", "done", "error"];

pub fn is_valid_status(s: &str) -> bool {
    VALID_STATUSES.contains(&s)
}

/// Location of the teams directory: `~/.cortex/teams/`.
pub fn teams_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("teams"))
}

/// Reject ids/names that contain path separators or `..` so callers can't
/// escape the teams dir. Empty values are also refused.
fn is_safe_id(id: &str) -> bool {
    let t = id.trim();
    !t.is_empty() && !t.contains('/') && !t.contains('\\') && !t.contains("..")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn short_id(prefix: &str) -> String {
    // 8-hex-char suffix: enough collision-resistance for per-user state without
    // dragging in the `uuid` crate for one call site. We mix three independent
    // sources so ids minted within the same millisecond can't collide:
    //   * the wall-clock time,
    //   * a process-lifetime atomic counter (distinct per call), and
    //   * `RandomState`, whose seed is randomized by the OS per construction.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(now_ms());
    hasher.write_u64(SEQ.fetch_add(1, Ordering::Relaxed));
    let rnd = hasher.finish();
    format!("{prefix}-{:08x}", (rnd & 0xffff_ffff) as u32)
}

/// List every team file under `~/.cortex/teams/*.json`, sorted by
/// `created_unix_ms` descending. Malformed files are skipped with a debug log.
pub fn list_teams() -> Vec<Team> {
    let Some(dir) = teams_dir() else {
        return Vec::new();
    };
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("teams: no dir ({}): {e}", dir.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("teams: read failed for {}: {e}", path.display());
                continue;
            }
        };
        match serde_json::from_str::<Team>(&raw) {
            Ok(t) => out.push(t),
            Err(e) => tracing::debug!("teams: parse failed for {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| b.created_unix_ms.cmp(&a.created_unix_ms));
    out
}

/// Load a single team by id. Returns `None` on missing / malformed.
pub fn get_team(id: &str) -> Option<Team> {
    if !is_safe_id(id) {
        return None;
    }
    let dir = teams_dir()?;
    let path = dir.join(format!("{id}.json"));
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Team>(&raw).ok()
}

/// Persist a team to disk. Creates the directory if needed.
pub fn save_team(team: &Team) -> anyhow::Result<()> {
    if !is_safe_id(&team.id) {
        anyhow::bail!("invalid team id '{}'", team.id);
    }
    let dir = teams_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", team.id));
    let body = serde_json::to_string_pretty(team)?;
    fs::write(&path, body)?;
    Ok(())
}

/// Build + persist a new team. Workers all start in `idle` with no task.
pub fn create_team(
    name: &str,
    manager_role: &str,
    worker_roles: &[String],
) -> anyhow::Result<Team> {
    if name.trim().is_empty() {
        anyhow::bail!("team name is required");
    }
    if manager_role.trim().is_empty() {
        anyhow::bail!("manager_role is required");
    }
    let now = now_ms();
    let id = short_id("team");
    let workers = worker_roles
        .iter()
        .filter(|r| !r.trim().is_empty())
        .map(|role| Worker {
            role: role.clone(),
            agent_id: short_id("wkr"),
            current_task: None,
            status: "idle".to_string(),
            started_unix_ms: now,
            last_event_unix_ms: now,
            message_count: 0,
            session_id: None,
            task_kind: None,
            task_difficulty: None,
            effective_model: None,
            projected_usd: None,
            lane_run_id: None,
        })
        .collect();
    let team = Team {
        id,
        name: name.trim().to_string(),
        manager_role: manager_role.trim().to_string(),
        workers,
        created_unix_ms: now,
        goal: None,
        run_status: None,
        last_run_unix_ms: None,
        plan_session_id: None,
        synthesis_session_id: None,
        spent_usd: None,
        budget_usd: None,
    };
    save_team(&team)?;
    Ok(team)
}

/// Mutate a single worker's status / current_task / counters and persist.
/// Returns the updated team so the UI can re-render without a follow-up fetch.
pub fn update_worker(
    team_id: &str,
    worker_id: &str,
    status: &str,
    current_task: Option<String>,
) -> anyhow::Result<Team> {
    patch_worker(team_id, worker_id, status, current_task, None, None, None)
}

/// Full worker patch: status (validated), task (`Some` sets, `None` keeps),
/// transcript session (`Some` sets, `None` keeps), and the slice-3 cost fields
/// `effective_model` / `projected_usd` (each `Some` sets, `None` keeps). The
/// team runner uses this to attach the run transcript and routing/cost record
/// when a worker finishes in one write — folding it into the existing patch
/// keeps the concurrent fan-out to a single read-modify-write per worker
/// instead of widening the cross-worker race with a second save. Everything
/// else goes through the narrower [`update_worker`].
pub fn patch_worker(
    team_id: &str,
    worker_id: &str,
    status: &str,
    current_task: Option<String>,
    session_id: Option<String>,
    effective_model: Option<String>,
    projected_usd: Option<f64>,
) -> anyhow::Result<Team> {
    if !is_valid_status(status) {
        anyhow::bail!(
            "invalid status '{status}' — expected one of {:?}",
            VALID_STATUSES
        );
    }
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    let now = now_ms();
    let mut found = false;
    for w in team.workers.iter_mut() {
        if w.agent_id == worker_id {
            // Only treat the bump as a "new" event when status or task actually
            // change — repeated polling shouldn't inflate `message_count`.
            let task_changed = current_task.as_deref() != w.current_task.as_deref();
            let status_changed = w.status != status;
            if status_changed || task_changed {
                w.message_count = w.message_count.saturating_add(1);
            }
            w.status = status.to_string();
            if current_task.is_some() {
                w.current_task = current_task.clone();
            }
            if session_id.is_some() {
                w.session_id = session_id.clone();
            }
            if effective_model.is_some() {
                w.effective_model = effective_model.clone();
            }
            if projected_usd.is_some() {
                w.projected_usd = projected_usd;
            }
            w.last_event_unix_ms = now;
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("worker '{worker_id}' not found in team '{team_id}'");
    }
    save_team(&team)?;
    Ok(team)
}

/// Persist a worker's planned subtask tags (`kind` + `difficulty`) from the
/// manager's plan. An empty/blank tag is stored as `None` (untagged). Unlike
/// [`patch_worker`] this never touches status/task/counters — slice 2 records
/// the tags alongside the `working` transition so slice 3 can read them back.
pub fn set_worker_tags(
    team_id: &str,
    worker_id: &str,
    kind: &str,
    difficulty: &str,
) -> anyhow::Result<Team> {
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    let mut found = false;
    for w in team.workers.iter_mut() {
        if w.agent_id == worker_id {
            w.task_kind = (!kind.trim().is_empty()).then(|| kind.trim().to_string());
            w.task_difficulty =
                (!difficulty.trim().is_empty()).then(|| difficulty.trim().to_string());
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("worker '{worker_id}' not found in team '{team_id}'");
    }
    save_team(&team)?;
    Ok(team)
}

/// Link a worker to the repo lane its code subtask was dispatched to
/// (orchestration slice 4). Mirrors [`set_worker_tags`]: never touches
/// status/task/counters, so the team runner can attach the `lane_runs` id
/// without widening the per-worker patch. A blank id clears the link; the link
/// is wiped by [`begin_run`] so a stale lane can't leak across runs.
pub fn set_worker_lane(
    team_id: &str,
    worker_id: &str,
    lane_run_id: &str,
) -> anyhow::Result<Team> {
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    let mut found = false;
    for w in team.workers.iter_mut() {
        if w.agent_id == worker_id {
            w.lane_run_id =
                (!lane_run_id.trim().is_empty()).then(|| lane_run_id.trim().to_string());
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("worker '{worker_id}' not found in team '{team_id}'");
    }
    save_team(&team)?;
    Ok(team)
}

/// Start a run: stamp the goal + `planning` state and reset every worker to a
/// clean `idle` slate (task/transcript cleared) so stale state from a prior
/// run can't masquerade as live progress.
pub fn begin_run(team_id: &str, goal: &str) -> anyhow::Result<Team> {
    if goal.trim().is_empty() {
        anyhow::bail!("goal is required");
    }
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    if team.workers.is_empty() {
        anyhow::bail!("team '{}' has no workers to assign", team.name);
    }
    let now = now_ms();
    team.goal = Some(goal.trim().to_string());
    team.run_status = Some("planning".to_string());
    team.last_run_unix_ms = Some(now);
    team.plan_session_id = None;
    team.spent_usd = None;
    team.synthesis_session_id = None;
    for w in team.workers.iter_mut() {
        w.status = "idle".to_string();
        w.current_task = None;
        w.session_id = None;
        w.task_kind = None;
        w.task_difficulty = None;
        w.effective_model = None;
        w.projected_usd = None;
        w.lane_run_id = None;
        w.started_unix_ms = now;
        w.last_event_unix_ms = now;
    }
    save_team(&team)?;
    Ok(team)
}

/// Advance the team-level run lifecycle (`planning | running | done | error`),
/// optionally attaching the manager's planning transcript.
pub fn set_run_status(
    team_id: &str,
    run_status: &str,
    plan_session_id: Option<String>,
) -> anyhow::Result<Team> {
    if !VALID_RUN_STATUSES.contains(&run_status) {
        anyhow::bail!(
            "invalid run_status '{run_status}' — expected one of {:?}",
            VALID_RUN_STATUSES
        );
    }
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    team.run_status = Some(run_status.to_string());
    if plan_session_id.is_some() {
        team.plan_session_id = plan_session_id;
    }
    save_team(&team)?;
    Ok(team)
}

/// Record the run's total estimated spend (slice 3). Called once after the
/// worker fan-out joins — single-threaded at that point, so no cross-worker
/// race. A negative value is rejected (cost is never negative).
pub fn set_spent_usd(team_id: &str, usd: f64) -> anyhow::Result<Team> {
    if usd < 0.0 || !usd.is_finite() {
        anyhow::bail!("invalid spent_usd {usd}");
    }
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    team.spent_usd = Some(usd);
    save_team(&team)?;
    Ok(team)
}

/// Set (or clear) the team's soft spend ceiling in USD (slice 6). `Some(v)`
/// requires a finite, non-negative value; `None` clears the budget. This is a
/// team-level setting the dashboard compares the run's projected spend against —
/// it never blocks or kills a run, so [`begin_run`] preserves it.
pub fn set_budget_usd(team_id: &str, usd: Option<f64>) -> anyhow::Result<Team> {
    if let Some(v) = usd {
        if v < 0.0 || !v.is_finite() {
            anyhow::bail!("invalid budget_usd {v}");
        }
    }
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    team.budget_usd = usd;
    save_team(&team)?;
    Ok(team)
}

/// Stamp the manager's synthesis transcript onto the team (slice 5). Called once
/// after the worker fan-out joins and the strong-tier synthesizer has merged
/// every worker's output into one result + a verification note. A blank id
/// clears the field (so a skipped synthesis leaves no stale link).
pub fn set_synthesis_session(team_id: &str, session_id: &str) -> anyhow::Result<Team> {
    let mut team =
        get_team(team_id).ok_or_else(|| anyhow::anyhow!("team '{team_id}' not found"))?;
    team.synthesis_session_id =
        (!session_id.trim().is_empty()).then(|| session_id.trim().to_string());
    save_team(&team)?;
    Ok(team)
}

/// Remove a team file. Missing files are a no-op (idempotent delete).
pub fn delete_team(id: &str) -> anyhow::Result<()> {
    if !is_safe_id(id) {
        anyhow::bail!("invalid team id '{id}'");
    }
    let Some(dir) = teams_dir() else {
        return Ok(());
    };
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests share the user's $HOME by default — serialize so they don't race on
    // the teams dir. Mirrors the pattern in `agents::roles::tests`.
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
            assert!(list_teams().is_empty());
        });
    }

    #[test]
    fn create_then_list_then_get() {
        with_temp_home(|| {
            let t = create_team(
                "refactor",
                "system-architect",
                &["coder".into(), "tester".into()],
            )
            .unwrap();
            assert_eq!(t.workers.len(), 2);
            assert!(t.workers.iter().all(|w| w.status == "idle"));

            let listed = list_teams();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, t.id);

            let fetched = get_team(&t.id).unwrap();
            assert_eq!(fetched, t);
        });
    }

    #[test]
    fn update_worker_bumps_count_and_persists() {
        with_temp_home(|| {
            let t = create_team("x", "manager", &["coder".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            let updated =
                update_worker(&t.id, &w_id, "working", Some("port the auth module".into()))
                    .unwrap();
            assert_eq!(updated.workers[0].status, "working");
            assert_eq!(updated.workers[0].message_count, 1);
            // Same status + task again => no bump.
            let again =
                update_worker(&t.id, &w_id, "working", Some("port the auth module".into()))
                    .unwrap();
            assert_eq!(again.workers[0].message_count, 1);
            // Status change => bump.
            let done = update_worker(&t.id, &w_id, "done", None).unwrap();
            assert_eq!(done.workers[0].message_count, 2);
            // And the disk copy reflects it.
            assert_eq!(get_team(&t.id).unwrap(), done);
        });
    }

    #[test]
    fn rejects_invalid_status() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            let err = update_worker(&t.id, &t.workers[0].agent_id, "spinning", None);
            assert!(err.is_err());
        });
    }

    #[test]
    fn delete_is_idempotent() {
        with_temp_home(|| {
            delete_team("team-doesnotexist").unwrap();
        });
    }

    #[test]
    fn begin_run_stamps_goal_and_resets_workers() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into(), "tester".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            // Dirty a worker so we can prove the reset.
            patch_worker(
                &t.id,
                &w_id,
                "done",
                Some("old task".into()),
                Some("session-old".into()),
                None,
                None,
            )
            .unwrap();

            let started = begin_run(&t.id, "ship the auth refactor").unwrap();
            assert_eq!(started.goal.as_deref(), Some("ship the auth refactor"));
            assert_eq!(started.run_status.as_deref(), Some("planning"));
            assert!(started.last_run_unix_ms.is_some());
            for w in &started.workers {
                assert_eq!(w.status, "idle");
                assert!(w.current_task.is_none());
                assert!(w.session_id.is_none());
            }
            // Empty goal / empty team are rejected.
            assert!(begin_run(&t.id, "   ").is_err());
            let lone = create_team("empty", "m", &[]).unwrap();
            assert!(begin_run(&lone.id, "goal").is_err());
        });
    }

    #[test]
    fn patch_worker_attaches_session() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            let updated =
                patch_worker(
                    &t.id,
                    &w_id,
                    "done",
                    Some("task".into()),
                    Some("session-abc".into()),
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(updated.workers[0].session_id.as_deref(), Some("session-abc"));
            // None leaves the session untouched.
            let again = patch_worker(&t.id, &w_id, "done", None, None, None, None).unwrap();
            assert_eq!(again.workers[0].session_id.as_deref(), Some("session-abc"));
        });
    }

    #[test]
    fn set_worker_tags_persists_and_clears() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            // Fresh workers carry no tags.
            assert!(t.workers[0].task_kind.is_none());
            assert!(t.workers[0].task_difficulty.is_none());

            let tagged = set_worker_tags(&t.id, &w_id, "code", "hard").unwrap();
            assert_eq!(tagged.workers[0].task_kind.as_deref(), Some("code"));
            assert_eq!(tagged.workers[0].task_difficulty.as_deref(), Some("hard"));
            // Persisted to disk.
            assert_eq!(get_team(&t.id).unwrap(), tagged);

            // Blank tags clear back to None.
            let cleared = set_worker_tags(&t.id, &w_id, "  ", "").unwrap();
            assert!(cleared.workers[0].task_kind.is_none());
            assert!(cleared.workers[0].task_difficulty.is_none());

            // A new run resets the tags even after they were set.
            set_worker_tags(&t.id, &w_id, "code", "hard").unwrap();
            let started = begin_run(&t.id, "do the thing").unwrap();
            assert!(started.workers[0].task_kind.is_none());
            assert!(started.workers[0].task_difficulty.is_none());

            // Unknown worker id is an error.
            assert!(set_worker_tags(&t.id, "wkr-nope", "chat", "easy").is_err());
        });
    }

    #[test]
    fn set_worker_lane_links_and_clears() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            assert!(t.workers[0].lane_run_id.is_none());

            let linked = set_worker_lane(&t.id, &w_id, "lane-e2e-123").unwrap();
            assert_eq!(linked.workers[0].lane_run_id.as_deref(), Some("lane-e2e-123"));
            assert_eq!(get_team(&t.id).unwrap(), linked);

            // Blank clears.
            let cleared = set_worker_lane(&t.id, &w_id, "  ").unwrap();
            assert!(cleared.workers[0].lane_run_id.is_none());

            // begin_run wipes any prior link.
            set_worker_lane(&t.id, &w_id, "lane-e2e-456").unwrap();
            let started = begin_run(&t.id, "next").unwrap();
            assert!(started.workers[0].lane_run_id.is_none());

            assert!(set_worker_lane(&t.id, "wkr-nope", "lane-x").is_err());
        });
    }

    #[test]
    fn set_run_status_validates_and_attaches_plan() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            assert!(set_run_status(&t.id, "sprinting", None).is_err());
            let done = set_run_status(&t.id, "done", Some("session-plan".into())).unwrap();
            assert_eq!(done.run_status.as_deref(), Some("done"));
            assert_eq!(done.plan_session_id.as_deref(), Some("session-plan"));
        });
    }

    #[test]
    fn patch_worker_records_cost_and_set_spent_usd_sums() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            let w_id = t.workers[0].agent_id.clone();
            // Fresh workers have no routing/cost record; the team none either.
            assert!(t.workers[0].effective_model.is_none());
            assert!(t.workers[0].projected_usd.is_none());
            assert!(t.spent_usd.is_none());

            let patched = patch_worker(
                &t.id,
                &w_id,
                "done",
                None,
                None,
                Some("claude-opus-4-8".into()),
                Some(0.42),
            )
            .unwrap();
            assert_eq!(patched.workers[0].effective_model.as_deref(), Some("claude-opus-4-8"));
            assert_eq!(patched.workers[0].projected_usd, Some(0.42));
            // None leaves the cost fields untouched (a later status-only patch).
            let again = patch_worker(&t.id, &w_id, "done", None, None, None, None).unwrap();
            assert_eq!(again.workers[0].effective_model.as_deref(), Some("claude-opus-4-8"));
            assert_eq!(again.workers[0].projected_usd, Some(0.42));

            let totaled = set_spent_usd(&t.id, 0.42).unwrap();
            assert_eq!(totaled.spent_usd, Some(0.42));
            // Persisted, and a zero (all-local) total is legitimate.
            assert_eq!(get_team(&t.id).unwrap().spent_usd, Some(0.42));
            assert_eq!(set_spent_usd(&t.id, 0.0).unwrap().spent_usd, Some(0.0));
            // Negative / non-finite spend is rejected.
            assert!(set_spent_usd(&t.id, -1.0).is_err());

            // begin_run wipes the run's cost record so stale spend can't leak.
            let started = begin_run(&t.id, "next run").unwrap();
            assert!(started.spent_usd.is_none());
            assert!(started.workers[0].effective_model.is_none());
            assert!(started.workers[0].projected_usd.is_none());
        });
    }

    #[test]
    fn set_budget_usd_sets_clears_and_survives_begin_run() {
        with_temp_home(|| {
            let t = create_team("x", "m", &["coder".into()]).unwrap();
            // Fresh teams carry no budget.
            assert!(t.budget_usd.is_none());

            let budgeted = set_budget_usd(&t.id, Some(2.50)).unwrap();
            assert_eq!(budgeted.budget_usd, Some(2.50));
            assert_eq!(get_team(&t.id).unwrap().budget_usd, Some(2.50));

            // A zero budget is legitimate (a "free runs only" ceiling).
            assert_eq!(set_budget_usd(&t.id, Some(0.0)).unwrap().budget_usd, Some(0.0));
            // None clears it.
            assert!(set_budget_usd(&t.id, None).unwrap().budget_usd.is_none());
            // Negative / non-finite is rejected.
            assert!(set_budget_usd(&t.id, Some(-1.0)).is_err());
            assert!(set_budget_usd(&t.id, Some(f64::NAN)).is_err());

            // Unlike the per-run cost fields, the budget is a team-level setting
            // that survives a new run (begin_run wipes spent_usd, not budget).
            set_budget_usd(&t.id, Some(5.0)).unwrap();
            let started = begin_run(&t.id, "next run").unwrap();
            assert_eq!(started.budget_usd, Some(5.0));
            assert!(started.spent_usd.is_none());

            // Unknown team is an error.
            assert!(set_budget_usd("team-nope", Some(1.0)).is_err());
        });
    }

    #[test]
    fn legacy_team_files_still_parse() {
        with_temp_home(|| {
            // A pre-run-fields team file (the on-disk shape before this module
            // grew goal/run_status/session_id) must still load.
            let dir = teams_dir().unwrap();
            fs::create_dir_all(&dir).unwrap();
            let legacy = r#"{
              "id": "team-deadbeef",
              "name": "legacy",
              "manager_role": "m",
              "workers": [{
                "role": "coder", "agent_id": "wkr-cafebabe", "status": "idle",
                "started_unix_ms": 1, "last_event_unix_ms": 1, "message_count": 0
              }],
              "created_unix_ms": 1
            }"#;
            fs::write(dir.join("team-deadbeef.json"), legacy).unwrap();
            let t = get_team("team-deadbeef").expect("legacy file should parse");
            assert!(t.goal.is_none() && t.run_status.is_none());
            assert!(t.workers[0].session_id.is_none());
        });
    }

    #[test]
    fn rejects_path_traversal() {
        with_temp_home(|| {
            assert!(get_team("../etc/passwd").is_none());
            assert!(delete_team("../evil").is_err());
        });
    }
}
