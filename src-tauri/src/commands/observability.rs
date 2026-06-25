use crate::observability::tracing_store::{
    AuditRow, HealthRow, IssueRow, SessionSearchHit, Trace, TraceEvent, TracingStore,
};
use tauri::State;

#[tauri::command]
pub async fn recent_traces(limit: Option<usize>, store: State<'_, TracingStore>) -> Result<Vec<Trace>, String> {
    let lim = limit.unwrap_or(20).clamp(1, 200);
    store.recent_traces(lim).map_err(|e| e.to_string())
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
