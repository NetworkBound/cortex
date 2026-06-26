//! Cortex Gateway capability surface — what the the gateway host gateway can route to.
//!
//! Calls `/v1/capabilities` first (newer gateway builds), falls back to
//! `/v1/models` when that endpoint is missing or returns garbage. Everything
//! beyond the model id list is best-effort — fields we can't infer default to
//! `false` / `None`.

use crate::app_state::AppState;
use crate::gateway::client::GatewayClient;
use serde::Serialize;
use std::time::Instant;
use tauri::State;

#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub owner: Option<String>,
    pub context_window: Option<u32>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProviderInfo {
    pub name: String,
    pub healthy: bool,
    pub last_check_ms: Option<u64>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Capabilities {
    pub models: Vec<ModelInfo>,
    pub providers: Vec<ProviderInfo>,
    pub gateway_version: Option<String>,
    /// Round-trip latency in ms for the capability probe itself. Surfaced so
    /// the UI can show "stale" data when the gateway is slow.
    pub fetched_in_ms: u64,
}

/// List available model/provider ids from the Cortex Gateway (`/v1/models`).
/// Backs the multi-provider selector — the user picks which of these run a turn.
#[tauri::command]
pub async fn list_gateway_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    let list = client.list_models().await.map_err(|e| format!("list models: {e}"))?;
    Ok(list.data.into_iter().map(|m| m.id).collect())
}

#[tauri::command]
pub async fn gateway_capabilities(state: State<'_, AppState>) -> Result<Capabilities, String> {
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let started = Instant::now();
    let caps = match client.capabilities().await {
        Ok(v) => parse_capabilities(&v),
        Err(_) => fallback_from_models(&client).await,
    };
    let elapsed = started.elapsed().as_millis() as u64;

    Ok(Capabilities {
        models: caps.models,
        providers: caps.providers,
        gateway_version: caps.gateway_version,
        fetched_in_ms: elapsed,
    })
}

struct ParsedCaps {
    models: Vec<ModelInfo>,
    providers: Vec<ProviderInfo>,
    gateway_version: Option<String>,
}

/// Pull what we can out of `/v1/capabilities`. Schema is loose — different
/// Gateway builds put fields in different places — so each lookup is defensive.
fn parse_capabilities(v: &serde_json::Value) -> ParsedCaps {
    let gateway_version = v
        .get("version")
        .and_then(|s| s.as_str())
        .or_else(|| v.get("gateway_version").and_then(|s| s.as_str()))
        .map(|s| s.to_string());

    let models_arr = v
        .get("models")
        .and_then(|m| m.as_array())
        .or_else(|| v.get("data").and_then(|d| d.as_array()))
        .cloned()
        .unwrap_or_default();

    let models = models_arr.iter().map(parse_model).collect();

    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().map(parse_provider).collect())
        .unwrap_or_default();

    ParsedCaps { models, providers, gateway_version }
}

fn parse_model(v: &serde_json::Value) -> ModelInfo {
    let id = v
        .get("id")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();

    let owner = v
        .get("owned_by")
        .and_then(|s| s.as_str())
        .or_else(|| v.get("owner").and_then(|s| s.as_str()))
        .or_else(|| v.get("provider").and_then(|s| s.as_str()))
        .map(|s| s.to_string());

    let context_window = v
        .get("context_window")
        .and_then(|n| n.as_u64())
        .or_else(|| v.get("context_length").and_then(|n| n.as_u64()))
        .or_else(|| v.get("max_context_tokens").and_then(|n| n.as_u64()))
        .map(|n| n.min(u32::MAX as u64) as u32);

    // Flags live in several shapes — flat booleans, or a `capabilities`
    // sub-object, or an array of strings.
    let caps_obj = v.get("capabilities");
    let supports_tools = bool_flag(v, caps_obj, &["supports_tools", "tools", "tool_use", "function_calling"]);
    let supports_vision = bool_flag(v, caps_obj, &["supports_vision", "vision", "multimodal"]);
    let supports_reasoning = bool_flag(v, caps_obj, &["supports_reasoning", "reasoning", "thinking"]);

    ModelInfo {
        id,
        owner,
        context_window,
        supports_tools,
        supports_vision,
        supports_reasoning,
    }
}

fn bool_flag(model: &serde_json::Value, caps: Option<&serde_json::Value>, keys: &[&str]) -> bool {
    for k in keys {
        if let Some(b) = model.get(*k).and_then(|x| x.as_bool()) {
            if b { return true; }
        }
        if let Some(c) = caps {
            if let Some(b) = c.get(*k).and_then(|x| x.as_bool()) {
                if b { return true; }
            }
            if let Some(arr) = c.as_array() {
                if arr.iter().any(|x| x.as_str() == Some(*k)) {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_provider(v: &serde_json::Value) -> ProviderInfo {
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .or_else(|| v.get("provider").and_then(|s| s.as_str()))
        .or_else(|| v.get("id").and_then(|s| s.as_str()))
        .unwrap_or("unknown")
        .to_string();

    let healthy = v
        .get("healthy")
        .and_then(|b| b.as_bool())
        .or_else(|| v.get("ok").and_then(|b| b.as_bool()))
        .or_else(|| {
            v.get("status")
                .and_then(|s| s.as_str())
                .map(|s| matches!(s, "ok" | "ready" | "healthy" | "up"))
        })
        .unwrap_or(false);

    let last_check_ms = v
        .get("last_check_ms")
        .and_then(|n| n.as_u64())
        .or_else(|| v.get("last_seen_ms").and_then(|n| n.as_u64()))
        .or_else(|| v.get("checked_at").and_then(|n| n.as_u64()));

    ProviderInfo { name, healthy, last_check_ms }
}

/// Fallback path: `/v1/capabilities` unavailable, build a minimal view from
/// `/v1/models`. No provider data, no version, no capability flags.
async fn fallback_from_models(client: &GatewayClient) -> ParsedCaps {
    let models = match client.list_models().await {
        Ok(list) => list
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                owner: None,
                context_window: None,
                supports_tools: false,
                supports_vision: false,
                supports_reasoning: false,
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    ParsedCaps {
        models,
        providers: Vec::new(),
        gateway_version: None,
    }
}
