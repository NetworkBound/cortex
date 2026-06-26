//! Tauri commands backing `~/.cortex/teams/*.json` — multi-agent team state
//! consumed by the orchestrator dashboard. All heavy lifting (validation,
//! disk IO, id minting) lives in `orchestrator::teams`; this file just adapts
//! the wire shape to async Tauri handlers and converts `anyhow::Error` to the
//! frontend-friendly `Result<_, String>` form.

use crate::app_state::AppState;
use crate::lanes::TERMINAL_STATUSES;
use crate::observability::tracing_store::TracingStore;
use crate::orchestrator::team_run::{self, LaneDispatch, LaneDispatcher};
use crate::orchestrator::teams::{self, Team};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

/// Cap on how long a single team code-lane may run before the worker is freed
/// (the lane keeps running on the gateway; the row stays in the Lanes tab). Matches
/// the chat worker's wall-clock budget in `team_run`.
const LANE_FOLLOW_TIMEOUT: Duration = Duration::from_secs(420);
/// Poll cadence while following a lane to a terminal state.
const LANE_POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Bridges the Tauri-free team engine to the Lanes machinery (slice 4): a
/// `Code`-tagged worker's subtask is dispatched as a gateway worktree lane via
/// [`crate::commands::multi_provider::dispatch_team_lane`], then followed to a
/// terminal state by observing the `lane_runs` row the watcher updates.
struct TeamLaneDispatcher {
    app: tauri::AppHandle,
    owner: String,
    repo: String,
    provider: String,
}

#[async_trait::async_trait]
impl LaneDispatcher for TeamLaneDispatcher {
    async fn dispatch(&self, goal: &str, task: &str) -> Result<LaneDispatch, String> {
        let instructions = format!("Team goal: {goal}");
        let record = crate::commands::multi_provider::dispatch_team_lane(
            &self.app,
            &self.owner,
            &self.repo,
            &self.provider,
            task,
            Some(&instructions),
        )
        .await?;
        let lane_run_id = record.run_id.clone();
        let branch = record
            .branch
            .as_deref()
            .map(|b| format!("Lane branch `{b}`. "))
            .unwrap_or_default();

        // A lane that failed to even start is already terminal.
        if record.status != "running" {
            return Ok(LaneDispatch {
                lane_run_id,
                status: record.status.clone(),
                outcome: format!(
                    "{branch}{}",
                    record.detail.as_deref().unwrap_or("the lane did not start")
                ),
                provider: self.provider.clone(),
            });
        }

        // Follow the lane to a terminal state by observing the row the detached
        // watcher updates (single source of truth — no second SSE consumer).
        let store = crate::commands::multi_provider::lane_store(&self.app);
        let deadline = Instant::now() + LANE_FOLLOW_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Ok(LaneDispatch {
                    lane_run_id,
                    status: "error".into(),
                    outcome: format!(
                        "{branch}still running after {}s — follow it in the Lanes tab.",
                        LANE_FOLLOW_TIMEOUT.as_secs()
                    ),
                    provider: self.provider.clone(),
                });
            }
            tokio::time::sleep(LANE_POLL_INTERVAL).await;
            let row = store
                .get(&lane_run_id)
                .map_err(|e| format!("lane lookup failed: {e}"))?;
            if let Some(r) = row {
                if TERMINAL_STATUSES.contains(&r.status.as_str()) {
                    return Ok(LaneDispatch {
                        lane_run_id,
                        status: r.status.clone(),
                        outcome: format!("{branch}{}", r.detail.as_deref().unwrap_or("completed")),
                        provider: self.provider.clone(),
                    });
                }
            }
        }
    }
}

/// Build a lane dispatcher for a run when a repo is bound (slice 4). `None` —
/// no repo, or an unparseable `owner/repo` — keeps code workers on the chat
/// path. Under `CORTEX_E2E` the provider is forced to the deterministic
/// `e2e-fake` lane producer so the probe never dials the gateway.
fn build_lane_dispatcher(
    app: &tauri::AppHandle,
    repo: Option<&str>,
    model: Option<&str>,
) -> Option<Arc<dyn LaneDispatcher>> {
    let (owner, repo) = parse_owner_repo(repo?)?;
    let provider = if crate::commands::e2e::e2e_enabled() {
        "e2e-fake".to_string()
    } else {
        model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("claude")
            .to_string()
    };
    Some(Arc::new(TeamLaneDispatcher {
        app: app.clone(),
        owner,
        repo,
        provider,
    }))
}

/// Parse a Gitea `owner/repo` (optional `.git` suffix) into its two parts.
/// Returns `None` for anything that isn't exactly one `/`-separated pair.
fn parse_owner_repo(raw: &str) -> Option<(String, String)> {
    let cleaned = raw.trim().trim_end_matches(".git");
    let (owner, repo) = cleaned.split_once('/')?;
    let (owner, repo) = (owner.trim(), repo.trim());
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// List every team under `~/.cortex/teams/*.json`, newest first. Missing
/// directory yields `[]` — never an error.
#[tauri::command]
pub async fn list_teams() -> Result<Vec<Team>, String> {
    Ok(teams::list_teams())
}

/// Load a single team by id. Errors if the file is missing or malformed.
#[tauri::command]
pub async fn get_team(id: String) -> Result<Team, String> {
    if id.trim().is_empty() {
        return Err("id is required".into());
    }
    teams::get_team(&id).ok_or_else(|| format!("team '{id}' not found"))
}

/// Create + persist a new team. Workers all start in `idle` with no task.
/// `budget_usd` is an optional soft spend ceiling (slice 6) — when present it's
/// stamped onto the new team so the dashboard can flag over-budget projections.
#[tauri::command]
pub async fn create_team(
    name: String,
    manager_role: String,
    worker_roles: Vec<String>,
    budget_usd: Option<f64>,
) -> Result<Team, String> {
    let team =
        teams::create_team(&name, &manager_role, &worker_roles).map_err(|e| e.to_string())?;
    match budget_usd {
        Some(_) => teams::set_budget_usd(&team.id, budget_usd).map_err(|e| e.to_string()),
        None => Ok(team),
    }
}

/// Mutate a single worker's status / current_task on `team_id`. Returns the
/// updated team so the UI can re-render without a follow-up `get_team` call.
#[tauri::command]
pub async fn update_team_worker(
    team_id: String,
    worker_id: String,
    status: String,
    current_task: Option<String>,
) -> Result<Team, String> {
    teams::update_worker(&team_id, &worker_id, &status, current_task)
        .map_err(|e| e.to_string())
}

/// Remove a team file. Missing files are a no-op.
#[tauri::command]
pub async fn delete_team(id: String) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("id is required".into());
    }
    if team_run::is_running(&id) {
        return Err("That team is mid-run — wait for it to finish before deleting.".into());
    }
    teams::delete_team(&id).map_err(|e| e.to_string())
}

/// Kick off a real team run: the manager plans one task per worker, then the
/// workers execute concurrently through the adapter registry (see
/// `orchestrator::team_run`). Returns the team already stamped `planning` —
/// the run itself continues detached (it survives tab switches; the dashboard
/// follows along via its poll + the `teams:updated` event). `model` is the
/// composer's current pick; `None` falls back to role models / default route.
#[tauri::command]
pub async fn run_team(
    team_id: String,
    goal: String,
    model: Option<String>,
    repo: Option<String>,
    budget_usd: Option<f64>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Team, String> {
    if goal.trim().is_empty() {
        return Err("Give the team a goal first.".into());
    }
    if !team_run::try_begin(&team_id) {
        return Err("That team is already running — one goal at a time.".into());
    }
    // Slice 6: the Assign-goal modal can set/adjust this run's soft budget. Apply
    // it before stamping `planning` so the returned team carries the new ceiling;
    // a bad value releases the guard rather than wedging the team.
    if budget_usd.is_some() {
        if let Err(e) = teams::set_budget_usd(&team_id, budget_usd) {
            team_run::finish(&team_id);
            return Err(e.to_string());
        }
    }
    // Stamp `planning` BEFORE returning so the UI reflects the run instantly;
    // on failure release the guard or it would wedge the team until restart.
    let team = match teams::begin_run(&team_id, &goal) {
        Ok(t) => t,
        Err(e) => {
            team_run::finish(&team_id);
            return Err(e.to_string());
        }
    };
    let _ = app.emit("teams:updated", &team_id);

    let registry = state.registry.clone();
    let store = app.state::<TracingStore>().inner().clone();
    // Slice 4: when a repo is bound, code-tagged subtasks edit it in a gateway
    // worktree lane; otherwise code workers stay on the chat path.
    let lane_dispatcher = build_lane_dispatcher(&app, repo.as_deref(), model.as_deref());
    tauri::async_runtime::spawn(async move {
        let notify_app = app.clone();
        let notify_id = team_id.clone();
        let result = team_run::execute_team(
            registry,
            store,
            team_id.clone(),
            goal,
            model,
            lane_dispatcher,
            move || {
                let _ = notify_app.emit("teams:updated", &notify_id);
            },
        )
        .await;
        if let Err(e) = result {
            tracing::warn!("team run {team_id} failed: {e}");
        }
        team_run::finish(&team_id);
    });
    Ok(team)
}

#[cfg(test)]
mod tests {
    use super::parse_owner_repo;

    #[test]
    fn parses_owner_repo_pairs() {
        assert_eq!(
            parse_owner_repo("NetworkBound/cortex"),
            Some(("NetworkBound".into(), "cortex".into()))
        );
        // Trims and strips a .git suffix.
        assert_eq!(
            parse_owner_repo("  owner/repo.git "),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn rejects_malformed_repo_specs() {
        for bad in ["", "noslash", "a/b/c", "/repo", "owner/", "   /   "] {
            assert_eq!(parse_owner_repo(bad), None, "{bad:?} should not parse");
        }
    }
}
