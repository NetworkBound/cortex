use crate::observability::crash::{self, CrashRow};
use crate::observability::tracing_store::TracingStore;
use tauri::State;

#[tauri::command]
pub async fn recent_crashes(
    limit: Option<usize>,
    store: State<'_, TracingStore>,
) -> Result<Vec<CrashRow>, String> {
    let lim = limit.unwrap_or(50).clamp(1, 500);
    let conn = store.shared_connection();
    crash::recent_crashes(&conn, lim).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn record_js_crash(
    kind: String,
    message: String,
    stack: Option<String>,
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    // Whitelist the kind so a misbehaving frontend can't pollute the table.
    let allowed = matches!(kind.as_str(), "js_error" | "js_unhandled_rejection");
    let kind = if allowed { kind.as_str() } else { "js_error" };
    let build = option_env!("CARGO_PKG_VERSION");
    let conn = store.shared_connection();
    crash::record_crash(&conn, kind, &message, stack.as_deref(), build)
        .map_err(|e| e.to_string())
}
