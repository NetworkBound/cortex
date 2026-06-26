use crate::app_state::AppState;
use crate::observability::tracing_store::TracingStore;
use crate::usage::{build_summary, fetch_gateway_status, GatewayStatus, UsageSummary};
use tauri::State;

#[tauri::command]
pub async fn usage_summary(store: State<'_, TracingStore>) -> Result<UsageSummary, String> {
    build_summary(&store).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn gateway_status(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    let base_url = state.config.read().gateway_base_url.clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    Ok(fetch_gateway_status(&base_url, &api_key).await)
}
