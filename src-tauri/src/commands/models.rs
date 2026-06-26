//! Aggregate model list for the composer model picker.
//!
//! Unifies three sources into one flat list the UI can group by `source`:
//!   - `claude-cli`: the local Claude Code CLI (when the binary is present).
//!   - `gateway`:    a curated catalog of the gateway's credential-pool models
//!                   (Gemini + OpenAI/Codex), plus anything the live
//!                   `/v1/models` call advertises (deduped by id).
//!   - `ollama`:     Ollama models discovered via `/api/tags` on the
//!                   configured server AND the local one (Cookbook pulls land
//!                   locally), deduped.
//!
//! Gateway/Ollama discovery is best-effort: any failure (gateway down, no key,
//! server unreachable) just omits those entries — it never fails the command.

use crate::app_state::AppState;
use crate::gateway::client::GatewayClient;
use crate::orchestrator::aliases;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize, Clone)]
pub struct ModelEntry {
    pub id: String,
    pub label: String,
    /// Which adapter/source serves this model: "claude-cli" | "gateway" | "ollama".
    pub source: String,
    pub available: bool,
}

/// Resolve the local `claude` binary the same way the adapter does.
fn claude_present() -> bool {
    crate::agents::claude_cli::claude_bin().is_some()
}

#[tauri::command]
pub async fn list_models(state: State<'_, AppState>) -> Result<Vec<ModelEntry>, String> {
    let mut out: Vec<ModelEntry> = Vec::new();

    // Local Claude Code CLI models — sourced from the unified catalog (the same
    // catalog `aliases::resolve_model`/`route` resolve against) so the picker and
    // the resolver can never disagree. Static slugs the CLI accepts via --model.
    if claude_present() {
        for (id, label) in aliases::models_for_source("claude-cli") {
            out.push(ModelEntry {
                id: id.to_string(),
                label: label.to_string(),
                source: "claude-cli".to_string(),
                available: true,
            });
        }
    }

    // Cortex Gateway models. The live `/v1/models` call under-reports (it only
    // advertises one virtual `gateway-agent`), so we seed the list with a curated
    // catalog of the credential pool's *real* models (Gemini + OpenAI/Codex),
    // then merge anything the live call returns — deduping by id so the same
    // model never appears twice. Claude is intentionally excluded here (served
    // by the local CLI adapter group above).
    let mut gateway_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, label) in aliases::models_for_source("gateway") {
        if gateway_seen.insert(id.to_string()) {
            out.push(ModelEntry {
                id: id.to_string(),
                label: label.to_string(),
                source: "gateway".to_string(),
                available: true,
            });
        }
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    if let Ok(list) = client.list_models().await {
        for m in list.data {
            if gateway_seen.insert(m.id.clone()) {
                out.push(ModelEntry {
                    id: m.id.clone(),
                    label: m.id,
                    source: "gateway".to_string(),
                    available: true,
                });
            }
        }
    }

    // Ollama models — best-effort discovery via `/api/tags` against BOTH the
    // configured server and the local one (deduped). Cookbook pulls land on
    // the local server even when the config points at a remote homelab box,
    // and the ollama adapter routes each tag to whichever server has it — so
    // the picker must surface the union. Any failure just omits that server's
    // entries; never error the command.
    let configured = cfg.ollama_base_url.trim_end_matches('/').to_string();
    let mut bases: Vec<&str> = Vec::new();
    if !configured.is_empty() {
        bases.push(configured.as_str());
    }
    if configured != crate::agents::ollama::LOCAL_OLLAMA {
        bases.push(crate::agents::ollama::LOCAL_OLLAMA);
    }
    let mut ollama_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ollama_tags: Vec<String> = Vec::new();
    for base in bases {
        for name in crate::agents::ollama::fetch_tags_at(base).await {
            if ollama_seen.insert(name.clone()) {
                ollama_tags.push(name);
            }
        }
    }
    if !ollama_tags.is_empty() {
        // Offer a single "Auto" entry that lets the ollama adapter pick the
        // best available model per task.
        out.push(ModelEntry {
            id: "ollama:auto".to_string(),
            label: "Auto · best local".to_string(),
            source: "ollama".to_string(),
            available: true,
        });
        for name in ollama_tags.into_iter().take(30) {
            out.push(ModelEntry {
                id: format!("ollama:{name}"),
                label: name.clone(),
                source: "ollama".to_string(),
                available: true,
            });
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curated gateway catalog feeds a HashSet dedup in `list_models`; any
    /// duplicate id there would silently drop a model. Guard it (the catalog now
    /// lives in `orchestrator::aliases`, but the picker still depends on it being
    /// duplicate-free and non-empty).
    #[test]
    fn gateway_catalog_has_no_duplicate_ids() {
        let mut seen = std::collections::HashSet::new();
        for (id, _label) in aliases::models_for_source("gateway") {
            assert!(seen.insert(id), "duplicate id in gateway catalog: {id}");
        }
    }

    /// Every catalog entry the picker surfaces must carry a non-empty id and
    /// label so the UI never renders a blank pill.
    #[test]
    fn picker_catalog_entries_are_populated() {
        for source in ["claude-cli", "gateway"] {
            for (id, label) in aliases::models_for_source(source) {
                assert!(!id.is_empty(), "empty id in {source} catalog");
                assert!(!label.is_empty(), "empty label for {id}");
            }
        }
    }
}
