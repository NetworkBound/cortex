use crate::observability::tracing_store::{
    AuditRow, HealthRow, IssueRow, ReliabilityReport, ReplayRunSummary, RunReplay, SessionSearchHit,
    Trace, TraceEvent, TracingStore,
};
use tauri::State;

/// Run Replay: recent runs for the picker (optionally scoped to a session).
#[tauri::command]
pub async fn list_replay_runs(
    session_id: Option<String>,
    limit: Option<usize>,
    store: State<'_, TracingStore>,
) -> Result<Vec<ReplayRunSummary>, String> {
    let lim = limit.unwrap_or(30).clamp(1, 200);
    store
        .list_replay_runs(session_id.as_deref(), lim)
        .map_err(|e| e.to_string())
}

/// Run Replay: full ordered timeline + metadata for one run.
#[tauri::command]
pub async fn run_replay(span_id: String, store: State<'_, TracingStore>) -> Result<RunReplay, String> {
    store.run_replay(&span_id).map_err(|e| e.to_string())
}

/// Run Replay: redacted JSONL export of one run (returned as a string; the UI
/// saves it). Redaction is applied at this single export choke-point.
#[tauri::command]
pub async fn export_run_replay(
    span_id: String,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    store.export_run_replay_jsonl(&span_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn recent_traces(limit: Option<usize>, store: State<'_, TracingStore>) -> Result<Vec<Trace>, String> {
    let lim = limit.unwrap_or(20).clamp(1, 200);
    store.recent_traces(lim).map_err(|e| e.to_string())
}

/// Agent Reliability Dashboard: aggregate run outcomes (success rate, latency
/// percentiles, tokens, estimated cost, top error class) per provider and per
/// model, optionally windowed to the last `window_hours`. Pure read-side.
#[tauri::command]
pub async fn reliability_summary(
    window_hours: Option<u32>,
    store: State<'_, TracingStore>,
) -> Result<ReliabilityReport, String> {
    // Clamp to a sane window; `None` / 0 means "all time".
    let since_ms = window_hours.filter(|h| *h > 0).map(|h| {
        let hours = h.min(24 * 365) as i64; // cap at 1 year
        chrono::Utc::now().timestamp_millis() - hours * 3_600_000
    });
    store.reliability_summary(since_ms).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn trace_events(trace_id: String, store: State<'_, TracingStore>) -> Result<Vec<TraceEvent>, String> {
    store.events_for_trace(&trace_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn homelab_health(store: State<'_, TracingStore>) -> Result<Vec<HealthRow>, String> {
    store.latest_health().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn recent_issues(limit: Option<usize>, store: State<'_, TracingStore>) -> Result<Vec<IssueRow>, String> {
    let lim = limit.unwrap_or(50).clamp(1, 500);
    store.recent_issues(lim).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn recent_audit(limit: Option<usize>, store: State<'_, TracingStore>) -> Result<Vec<AuditRow>, String> {
    let lim = limit.unwrap_or(100).clamp(1, 1000);
    store.recent_audit(lim).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_sessions(
    query: String,
    limit: Option<usize>,
    store: State<'_, TracingStore>,
) -> Result<Vec<SessionSearchHit>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let lim = limit.unwrap_or(50).clamp(1, 500) as i64;
    store.search_messages(trimmed, lim).map_err(|e| e.to_string())
}
