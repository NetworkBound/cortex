//! Homelab Model Fabric commands — CRUD + probe for user-defined
//! OpenAI-compatible endpoints. Explicit routing only (an endpoint's adapter id
//! rides the existing exact-adapter-id branch in `orchestrator::route`); this
//! module never touches `route()`'s safety branches.

use crate::agents::custom_endpoint::{
    load_endpoints, normalize_id, probe, save_endpoints, CustomEndpointAgent, EndpointCfg,
    ProbeResult,
};
use crate::app_state::AppState;
use crate::commands::keyvault;
use crate::observability::tracing_store::TracingStore;
use std::sync::Arc;
use tauri::State;

const VAULT_KEY_LABEL: &str = "api-key";

/// Validate + normalize a user base URL into an OpenAI base ending in `/v1`.
/// Accepts only `http`/`https` with a host. Returns a user-readable error.
fn normalize_base_url(raw: &str) -> Result<String, String> {
    let t = raw.trim().trim_end_matches('/');
    let rest = t
        .strip_prefix("http://")
        .or_else(|| t.strip_prefix("https://"))
        .ok_or_else(|| "endpoint URL must start with http:// or https://".to_string())?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.starts_with(':') {
        return Err("endpoint URL is missing a host".into());
    }
    // Ensure the OpenAI `/v1` segment is present (append if the user omitted it).
    Ok(if t.contains("/v1") { t.to_string() } else { format!("{t}/v1") })
}

/// List all configured Model Fabric endpoints (keys are NOT included).
#[tauri::command]
pub async fn list_endpoints() -> Result<Vec<EndpointCfg>, String> {
    Ok(load_endpoints())
}

/// Create or update an endpoint. Validates the URL, normalizes the id to
/// `fabric-<slug>` (which can never shadow a built-in adapter), persists the
/// config, live-(re)registers the adapter, and stores the optional API key in
/// the KeyVault (never in the config file).
#[tauri::command]
pub async fn save_endpoint(
    label: String,
    base_url: String,
    kind: Option<String>,
    enabled: Option<bool>,
    id: Option<String>,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<EndpointCfg>, String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("label must not be empty".into());
    }
    let base_url = normalize_base_url(&base_url)?;
    // Prefer an explicit id (edit), else derive from the label (create).
    let id = normalize_id(id.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&label))
        .ok_or_else(|| "could not derive a valid id from the label".to_string())?;
    let kind = match kind.as_deref() {
        Some("remote") => "remote",
        _ => "local",
    }
    .to_string();
    let enabled = enabled.unwrap_or(true);

    let cfg = EndpointCfg { id: id.clone(), label, base_url, kind, enabled };

    // Persist config (upsert on id).
    let mut list = load_endpoints();
    if let Some(existing) = list.iter_mut().find(|e| e.id == cfg.id) {
        *existing = cfg.clone();
    } else {
        list.push(cfg.clone());
    }
    save_endpoints(&list).map_err(|e| e.to_string())?;

    // Store the key in the vault if provided (await OUTSIDE the registry guard).
    if let Some(key) = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        keyvault::vault_set(cfg.id.clone(), VAULT_KEY_LABEL.to_string(), key).await?;
    }

    // Live-(re)register the adapter — keep the write guard scope tight and NEVER
    // hold it across an await (the async Send hazard chat.rs documents).
    {
        let mut reg = state.registry.write();
        if cfg.enabled {
            reg.register(Arc::new(CustomEndpointAgent::new(cfg.clone())));
        } else {
            reg.unregister(&cfg.id);
        }
    }

    Ok(list)
}

/// Delete an endpoint: remove from config, deregister the adapter, and remove
/// its stored key (best-effort).
#[tauri::command]
pub async fn delete_endpoint(
    id: String,
    state: State<'_, AppState>,
) -> Result<Vec<EndpointCfg>, String> {
    let mut list = load_endpoints();
    let before = list.len();
    list.retain(|e| e.id != id);
    if list.len() == before {
        return Err(format!("no endpoint with id {id}"));
    }
    save_endpoints(&list).map_err(|e| e.to_string())?;

    {
        let mut reg = state.registry.write();
        reg.unregister(&id);
    }
    // Best-effort key removal (ignore "no key" errors).
    let _ = keyvault::vault_remove(id.clone(), VAULT_KEY_LABEL.to_string()).await;

    Ok(list)
}

/// Probe an endpoint (reachability + latency + discovered models). ALWAYS
/// UNAUTHENTICATED — never attaches the stored key, so a hostile/mistyped URL
/// can't harvest it. Records a health sample so the reliability/observability
/// surfaces show endpoint history. `base_url` is validated the same way as save.
#[tauri::command]
pub async fn probe_endpoint(
    base_url: String,
    id: Option<String>,
    store: State<'_, TracingStore>,
) -> Result<ProbeResult, String> {
    let base_url = normalize_base_url(&base_url)?;
    let result = probe(&base_url).await;
    if let Some(id) = id.filter(|s| !s.trim().is_empty()) {
        let _ = store.record_health(&format!("fabric:{id}"), result.ok, result.latency_ms, None);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_url_requires_scheme_and_host_and_adds_v1() {
        assert_eq!(normalize_base_url("http://192.168.1.5:8000").unwrap(), "http://192.168.1.5:8000/v1");
        assert_eq!(normalize_base_url("http://host:8000/v1/").unwrap(), "http://host:8000/v1");
        assert_eq!(normalize_base_url("https://api.example.com/v1").unwrap(), "https://api.example.com/v1");
        assert!(normalize_base_url("ftp://x").is_err());
        assert!(normalize_base_url("http://").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }
}
