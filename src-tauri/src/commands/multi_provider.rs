//! Multi-provider parallel runs — dispatch + tracking (Phase A/C).
//!
//! Launches one isolated gateway run per selected provider on the SAME task.
//! Each run carries a `cortex_worktree` so the gateway clones the Gitea project and
//! runs that provider's agent in its own git worktree (branch
//! `cortex/<run>/<provider>`) — parallel providers editing one project never
//! collide. Honored by the deployed `/v1/runs` change
//! (gateway-integration/DEPLOYED-runs-cwd.md).
//!
//! Every dispatched lane is persisted to the `lane_runs` table and followed by
//! a detached watcher that folds the run's SSE events into the row (see
//! `crate::lanes`), emitting `lanes:updated` on each transition — the pane
//! re-reads `list_lane_runs`, so runs survive tab switches and app restarts
//! instead of vanishing fire-and-forget.

use crate::app_state::AppState;
use crate::gateway::client::{CortexWorktree, GatewayClient, RunRequest, RunStreamItem};
use crate::lanes::{lane_transition, LaneRunRecord, LaneStore};
use crate::observability::tracing_store::TracingStore;
use serde::Deserialize;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Event the lanes pane listens on; payload is the affected run id.
const LANES_UPDATED: &str = "lanes:updated";

/// E2E-only fake providers (CORTEX_E2E=1): `e2e-fake` settles `done` through
/// the real watcher/transition pipeline without dialing the gateway; `e2e-fake-hang`
/// stays `running` so the probe can exercise `stop_lane_run`;
/// `e2e-fake-interrupt` is born `interrupted` with no watcher (the shape the
/// startup sweep leaves behind) so the probe can exercise `reattach_lane_run`.
const E2E_FAKE_PROVIDER: &str = "e2e-fake";
const E2E_HANG_PROVIDER: &str = "e2e-fake-hang";
const E2E_INTERRUPT_PROVIDER: &str = "e2e-fake-interrupt";
const E2E_RUN_PREFIX: &str = "lane-e2e-";

#[derive(Debug, Deserialize)]
pub struct ProviderLanesArgs {
    /// Gitea project the providers work on (the gateway clones `<owner>/<repo>`).
    pub owner: String,
    pub repo: String,
    /// Provider/model ids to run in parallel (from `list_gateway_models`).
    pub providers: Vec<String>,
    pub input: String,
    #[serde(default)]
    pub instructions: Option<String>,
}

pub(crate) fn lane_store(app: &tauri::AppHandle) -> LaneStore {
    LaneStore::new(app.state::<TracingStore>().inner().shared_connection())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Start one worktree-isolated run per provider. Best-effort per lane: a lane
/// that fails to start is persisted as an `error` row instead of aborting the
/// others. Returns the persisted records (the same rows `list_lane_runs`
/// serves) so the pane renders from one shape everywhere.
#[tauri::command]
pub async fn run_provider_lanes(
    args: ProviderLanesArgs,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<LaneRunRecord>, String> {
    if args.providers.is_empty() {
        return Err("no providers selected".into());
    }
    if args.owner.trim().is_empty() || args.repo.trim().is_empty() {
        return Err("owner and repo are required".into());
    }
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    let store = lane_store(&app);

    let mut out = Vec::with_capacity(args.providers.len());
    for provider in &args.providers {
        match start_lane_for_provider(&app, &store, &client, &args, provider).await {
            Ok(r) => out.push(r),
            Err(e) => return Err(e),
        }
    }
    let _ = app.emit(LANES_UPDATED, "");
    Ok(out)
}

/// Start one lane for `provider`: the e2e-fake producer under `CORTEX_E2E` (+
/// a reserved provider id), the real gateway worktree run otherwise. Shared by
/// `run_provider_lanes` (parallel fan-out) and `dispatch_team_lane` (the team
/// orchestrator's per-worker code dispatch) so both reach lanes through one
/// path.
async fn start_lane_for_provider(
    app: &tauri::AppHandle,
    store: &LaneStore,
    client: &GatewayClient,
    args: &ProviderLanesArgs,
    provider: &str,
) -> Result<LaneRunRecord, String> {
    if crate::commands::e2e::e2e_enabled()
        && (provider == E2E_FAKE_PROVIDER
            || provider == E2E_HANG_PROVIDER
            || provider == E2E_INTERRUPT_PROVIDER)
    {
        start_fake_lane(app, store, args, provider)
    } else {
        start_gateway_lane(app, store, client, args, provider).await
    }
}

/// Dispatch ONE worktree lane on behalf of the team orchestrator (slice 4):
/// a `Code`-tagged worker's subtask becomes a first-class `lane_runs` row,
/// followed by the same watcher `run_provider_lanes` uses, and browsable in the
/// Lanes tab. Returns the persisted record; the caller follows it to terminal
/// via the lane store. Reuses [`start_lane_for_provider`] so the e2e-fake path
/// works identically here.
pub(crate) async fn dispatch_team_lane(
    app: &tauri::AppHandle,
    owner: &str,
    repo: &str,
    provider: &str,
    input: &str,
    instructions: Option<&str>,
) -> Result<LaneRunRecord, String> {
    if owner.trim().is_empty() || repo.trim().is_empty() {
        return Err("owner and repo are required".into());
    }
    let (base_url, api_key) = {
        let state = app.state::<AppState>();
        let cfg = state.config.read().clone();
        (cfg.gateway_base_url, AppState::get_gateway_api_key().unwrap_or_default())
    };
    let client = GatewayClient::new(base_url, api_key);
    let store = lane_store(app);
    let args = ProviderLanesArgs {
        owner: owner.to_string(),
        repo: repo.to_string(),
        providers: vec![provider.to_string()],
        input: input.to_string(),
        instructions: instructions.map(|s| s.to_string()),
    };
    let record = start_lane_for_provider(app, &store, &client, &args, provider).await?;
    let _ = app.emit(LANES_UPDATED, &record.run_id);
    Ok(record)
}

/// Dispatch one real gateway lane: start the run, persist the row, attach the
/// SSE watcher. A failed start still persists (status `error`) so the history
/// is honest about what was attempted.
async fn start_gateway_lane(
    app: &tauri::AppHandle,
    store: &LaneStore,
    client: &GatewayClient,
    args: &ProviderLanesArgs,
    provider: &str,
) -> Result<LaneRunRecord, String> {
    let req = RunRequest {
        input: args.input.clone(),
        instructions: args.instructions.clone(),
        previous_response_id: None,
        conversation_history: None,
        model: Some(provider.to_string()),
        reasoning_effort: None,
        cortex_worktree: Some(CortexWorktree {
            owner: args.owner.clone(),
            repo: args.repo.clone(),
            provider: provider.to_string(),
        }),
    };
    // Distinct session per lane so concurrent providers don't share state.
    let session = format!("lane-{}-{}", provider, Uuid::new_v4());
    let now = now_ms();
    let record = match client.start_run(req, Some(&session), None).await {
        Ok(run_id) => LaneRunRecord {
            branch: Some(format!("cortex/{run_id}/{provider}")),
            run_id,
            provider: provider.to_string(),
            owner: args.owner.clone(),
            repo: args.repo.clone(),
            task: args.input.clone(),
            status: "running".into(),
            detail: Some("dispatched to the gateway".into()),
            created_at: now,
            updated_at: now,
            merged_at: None,
        },
        Err(e) => LaneRunRecord {
            run_id: format!("lane-failed-{}", Uuid::new_v4()),
            provider: provider.to_string(),
            owner: args.owner.clone(),
            repo: args.repo.clone(),
            task: args.input.clone(),
            branch: None,
            status: "error".into(),
            detail: Some(format!("failed to start: {e}")),
            created_at: now,
            updated_at: now,
            merged_at: None,
        },
    };
    store.insert(&record).map_err(|e| e.to_string())?;
    if record.status == "running" {
        spawn_lane_watcher(app.clone(), client.clone(), record.run_id.clone());
    }
    Ok(record)
}

/// Follow one lane's SSE stream, folding events into its row. Detached on
/// purpose: it outlives the dispatching IPC call and keeps updating the row
/// while the user is on other tabs. If the stream ends without a terminal
/// event (gateway restart, network drop), the lane is stamped `error` — the
/// terminal-wins guard in `update_status` makes that a no-op when a real
/// `Done` already landed.
fn spawn_lane_watcher(app: tauri::AppHandle, client: GatewayClient, run_id: String) {
    tauri::async_runtime::spawn(async move {
        let (tx, rx) = mpsc::channel::<RunStreamItem>(64);
        let stream_client = client.clone();
        let stream_run_id = run_id.clone();
        let stream = tauri::async_runtime::spawn(async move {
            stream_client.run_event_stream(&stream_run_id, tx).await
        });
        let saw_terminal = apply_lane_stream(&app, &run_id, rx).await;
        if !saw_terminal {
            let detail = match stream.await {
                Ok(Err(e)) => format!("lost the event stream: {e}"),
                _ => "event stream ended before the run finished".to_string(),
            };
            let store = lane_store(&app);
            if store.update_status(&run_id, "error", Some(&detail)).unwrap_or(false) {
                let _ = app.emit(LANES_UPDATED, &run_id);
            }
        }
    });
}

/// Consume a lane's stream, persisting each transition and emitting
/// `lanes:updated` only when a row actually changed. Returns whether a
/// terminal transition (`done`) was applied. Shared by the real gateway
/// watcher and the e2e fake producer, so the probe exercises the exact
/// transition pipeline production uses.
async fn apply_lane_stream(
    app: &tauri::AppHandle,
    run_id: &str,
    mut rx: mpsc::Receiver<RunStreamItem>,
) -> bool {
    let store = lane_store(app);
    let mut saw_terminal = false;
    while let Some(item) = rx.recv().await {
        let Some((status, detail)) = lane_transition(&item) else { continue };
        if store.update_status(run_id, &status, detail.as_deref()).unwrap_or(false) {
            let _ = app.emit(LANES_UPDATED, run_id);
        }
        if status == "done" {
            saw_terminal = true;
        }
    }
    saw_terminal
}

/// E2E-only lane (never reachable in a normal session — gated on
/// `CORTEX_E2E` AND the reserved provider ids): synthesizes a deterministic
/// event stream through the same `apply_lane_stream` pipeline. `e2e-fake`
/// settles `done` in ~0.5s; `e2e-fake-hang` keeps the lane `running` so the
/// probe can prove `stop_lane_run`.
fn start_fake_lane(
    app: &tauri::AppHandle,
    store: &LaneStore,
    args: &ProviderLanesArgs,
    provider: &str,
) -> Result<LaneRunRecord, String> {
    let run_id = format!("{E2E_RUN_PREFIX}{}", Uuid::new_v4());
    let now = now_ms();
    // Born interrupted, no watcher — exactly what the startup sweep leaves
    // behind after a mid-run crash, so the probe can prove the reattach path.
    if provider == E2E_INTERRUPT_PROVIDER {
        let record = LaneRunRecord {
            branch: Some(format!("cortex/{run_id}/{provider}")),
            run_id,
            provider: provider.to_string(),
            owner: args.owner.clone(),
            repo: args.repo.clone(),
            task: args.input.clone(),
            status: "interrupted".into(),
            detail: Some("Cortex restarted while this lane was running (e2e fake)".into()),
            created_at: now,
            updated_at: now,
            merged_at: None,
        };
        store.insert(&record).map_err(|e| e.to_string())?;
        return Ok(record);
    }
    let record = LaneRunRecord {
        branch: Some(format!("cortex/{run_id}/{provider}")),
        run_id: run_id.clone(),
        provider: provider.to_string(),
        owner: args.owner.clone(),
        repo: args.repo.clone(),
        task: args.input.clone(),
        status: "running".into(),
        detail: Some("dispatched (e2e fake)".into()),
        created_at: now,
        updated_at: now,
        merged_at: None,
    };
    store.insert(&record).map_err(|e| e.to_string())?;

    let hang = provider == E2E_HANG_PROVIDER;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (tx, rx) = mpsc::channel::<RunStreamItem>(8);
        let producer = tauri::async_runtime::spawn(async move {
            let _ = tx
                .send(RunStreamItem::ToolStarted { tool: "e2e".into(), preview: None })
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let _ = tx.send(RunStreamItem::Status("working".into())).await;
            if hang {
                // Keep the lane running long enough for the probe to stop it;
                // dropping tx afterwards triggers the stream-ended fallback,
                // which the terminal-wins guard turns into a no-op post-stop.
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let _ = tx.send(RunStreamItem::Done).await;
            }
        });
        let _ = apply_lane_stream(&app, &run_id, rx).await;
        producer.abort();
    });
    Ok(record)
}

/// The persisted lane history, newest first — the pane's source of truth.
#[tauri::command]
pub async fn list_lane_runs(
    limit: Option<u32>,
    app: tauri::AppHandle,
) -> Result<Vec<LaneRunRecord>, String> {
    lane_store(&app).list(limit).map_err(|e| e.to_string())
}

/// Stop a running lane: tell the gateway to stop the run, then stamp the row
/// `stopped`. Gateway failures propagate — a lane the gateway is still running
/// must not be displayed as stopped.
#[tauri::command]
pub async fn stop_lane_run(
    run_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = lane_store(&app);
    let lane = store
        .get(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("lane '{run_id}' not found"))?;
    if lane.status != "running" {
        return Err(format!("lane is already {} — nothing to stop", lane.status));
    }
    // E2E fake lanes have no gateway run to stop.
    if !run_id.starts_with(E2E_RUN_PREFIX) {
        let cfg = state.config.read().clone();
        let api_key = AppState::get_gateway_api_key().unwrap_or_default();
        let client = GatewayClient::new(cfg.gateway_base_url, api_key);
        client.stop_run(&run_id).await.map_err(|e| e.to_string())?;
    }
    if store
        .update_status(&run_id, "stopped", Some("stopped from Cortex"))
        .map_err(|e| e.to_string())?
    {
        let _ = app.emit(LANES_UPDATED, &run_id);
    }
    Ok(())
}

/// Remove a settled lane from the history. Running lanes must be stopped
/// first — deleting the row would orphan a live watcher and hide a run that
/// is still mutating the project on the gateway.
#[tauri::command]
pub async fn delete_lane_run(run_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let store = lane_store(&app);
    let lane = store
        .get(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("lane '{run_id}' not found"))?;
    if lane.status == "running" {
        return Err("That lane is still running — stop it before removing it.".into());
    }
    store.delete(&run_id).map_err(|e| e.to_string())?;
    let _ = app.emit(LANES_UPDATED, &run_id);
    Ok(())
}

/// Re-follow an `interrupted` lane's event stream ("Reattach"). The row flips
/// back to `running` only once the reattached stream actually delivers an
/// event (the run is provably live on the gateway again); a stream that ends
/// without ever producing one leaves the status `interrupted` and just
/// records an honest detail line. Returns immediately — progress arrives via
/// `lanes:updated` like every other watcher transition.
#[tauri::command]
pub async fn reattach_lane_run(
    run_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = lane_store(&app);
    let lane = store
        .get(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("lane '{run_id}' not found"))?;
    if lane.status != "interrupted" {
        return Err(format!(
            "Only interrupted lanes can reattach — this one is {}.",
            lane.status
        ));
    }
    if crate::commands::e2e::e2e_enabled() && run_id.starts_with(E2E_RUN_PREFIX) {
        spawn_fake_reattach(app.clone(), run_id);
        return Ok(());
    }
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    spawn_reattach_watcher(app.clone(), client, run_id);
    Ok(())
}

/// Reattach variant of [`spawn_lane_watcher`]: the lane starts `interrupted`,
/// so nothing may be stamped until the stream proves itself. Three outcomes:
/// no event ever → still `interrupted`, detail says why; events then a clean
/// `Done` → `done`; events then stream loss → `error` (same fallback as the
/// primary watcher — the lane really was running again when we lost it).
fn spawn_reattach_watcher(app: tauri::AppHandle, client: GatewayClient, run_id: String) {
    tauri::async_runtime::spawn(async move {
        let (tx, rx) = mpsc::channel::<RunStreamItem>(64);
        let stream_client = client.clone();
        let stream_run_id = run_id.clone();
        let stream = tauri::async_runtime::spawn(async move {
            stream_client.run_event_stream(&stream_run_id, tx).await
        });
        let (saw_any, saw_terminal) = apply_reattached_stream(&app, &run_id, rx).await;
        let store = lane_store(&app);
        if !saw_any {
            let detail = match stream.await {
                Ok(Err(e)) => format!("reattach failed — {e}"),
                _ => "reattach found no live stream — the run likely ended while Cortex \
                      was closed"
                    .to_string(),
            };
            if store.set_detail(&run_id, &detail).unwrap_or(false) {
                let _ = app.emit(LANES_UPDATED, &run_id);
            }
        } else if !saw_terminal {
            let detail = match stream.await {
                Ok(Err(e)) => format!("lost the event stream: {e}"),
                _ => "event stream ended before the run finished".to_string(),
            };
            if store.update_status(&run_id, "error", Some(&detail)).unwrap_or(false) {
                let _ = app.emit(LANES_UPDATED, &run_id);
            }
        }
    });
}

/// Like [`apply_lane_stream`], plus the reattach flip: the FIRST delivered
/// item (any item — even ones `lane_transition` ignores) proves the run is
/// live and moves `interrupted` → `running`. Returns (saw any event, saw
/// terminal `done`).
async fn apply_reattached_stream(
    app: &tauri::AppHandle,
    run_id: &str,
    mut rx: mpsc::Receiver<RunStreamItem>,
) -> (bool, bool) {
    let store = lane_store(app);
    let mut saw_any = false;
    let mut saw_terminal = false;
    while let Some(item) = rx.recv().await {
        if !saw_any {
            saw_any = true;
            if store.reattach_to_running(run_id).unwrap_or(false) {
                let _ = app.emit(LANES_UPDATED, run_id);
            }
        }
        let Some((status, detail)) = lane_transition(&item) else { continue };
        if store.update_status(run_id, &status, detail.as_deref()).unwrap_or(false) {
            let _ = app.emit(LANES_UPDATED, run_id);
        }
        if status == "done" {
            saw_terminal = true;
        }
    }
    (saw_any, saw_terminal)
}

/// E2E-only reattach stream (gated by the caller on `CORTEX_E2E` + the
/// reserved run-id prefix): a short deterministic burst through
/// [`apply_reattached_stream`], so the probe drives the exact
/// interrupted → running → done pipeline production uses.
fn spawn_fake_reattach(app: tauri::AppHandle, run_id: String) {
    tauri::async_runtime::spawn(async move {
        let (tx, rx) = mpsc::channel::<RunStreamItem>(8);
        let producer = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let _ = tx.send(RunStreamItem::Status("picked the run back up".into())).await;
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let _ = tx.send(RunStreamItem::Done).await;
        });
        let _ = apply_reattached_stream(&app, &run_id, rx).await;
        producer.abort();
    });
}
