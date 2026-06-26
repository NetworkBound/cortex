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

/// Build gateway-source model entries from the curated catalog plus whatever
/// the live `/v1/models` call returned. `live` is `None` when the call
/// failed (gateway unconfigured, unreachable, or rejected the key) — in that
/// case every curated entry is honestly marked `available: false` rather
/// than the hardcoded `true` this used to carry regardless of reachability.
/// Pulled out of `list_models` so this exact honesty contract has a direct
/// unit test that doesn't need a live gateway or Tauri `AppState`.
fn gateway_entries(live: Option<Vec<crate::gateway::client::ModelInfo>>) -> Vec<ModelEntry> {
    let available = live.is_some();
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, label) in aliases::models_for_source("gateway") {
        if seen.insert(id.to_string()) {
            out.push(ModelEntry {
                id: id.to_string(),
                label: label.to_string(),
                source: "gateway".to_string(),
                available,
            });
        }
    }
    if let Some(list) = live {
        for m in list {
            if seen.insert(m.id.clone()) {
                out.push(ModelEntry {
                    id: m.id.clone(),
                    label: m.id,
                    source: "gateway".to_string(),
                    available: true,
                });
            }
        }
    }
    out
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
    //
    // IMPORTANT: the live call runs FIRST and its success/failure is what marks
    // every gateway-source entry (curated + merged) `available`. A brand-new
    // install with no gateway URL/key configured — or a gateway that's simply
    // down — must NOT show these as available: that would be exactly the fake
    // "ready" status this picker must never present (mirrors the Ollama
    // section below, whose entries only ever appear after a real `/api/tags`
    // probe succeeds).
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);
    let live = client.list_models().await.ok().map(|list| list.data);
    out.extend(gateway_entries(live));

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

    /// The bug this guards: the curated gateway catalog used to be pushed
    /// with `available: true` unconditionally, regardless of whether the
    /// gateway was ever configured or reachable — an unconfigured/dead
    /// gateway showed as "ready" in the model picker and onboarding's model
    /// count. When the live probe fails (`live: None`), every curated entry
    /// must honestly report `available: false`.
    #[test]
    fn gateway_catalog_is_unavailable_when_live_probe_fails() {
        let entries = gateway_entries(None);
        assert!(!entries.is_empty(), "curated catalog should still be listed");
        for e in &entries {
            assert!(
                !e.available,
                "entry {} must be unavailable when the gateway is unreachable",
                e.id
            );
        }
    }

    /// When the live probe succeeds, curated entries become available, and
    /// anything the live call reports that ISN'T in the curated catalog is
    /// merged in (also available), deduped by id.
    #[test]
    fn gateway_catalog_is_available_and_merges_live_entries_when_probe_succeeds() {
        let live_only_id = "gateway-live-only-model-xyz".to_string();
        let live = vec![crate::gateway::client::ModelInfo { id: live_only_id.clone() }];
        let entries = gateway_entries(Some(live));

        let curated_count = aliases::models_for_source("gateway").len();
        assert_eq!(entries.len(), curated_count + 1, "curated + 1 merged live-only entry");
        assert!(entries.iter().all(|e| e.available), "all entries available on a successful probe");
        assert!(
            entries.iter().any(|e| e.id == live_only_id),
            "live-only model must be merged in"
        );
    }

    /// A live entry whose id duplicates a curated one must not create a
    /// second row (dedup-by-id must survive the reordering that fixed the
    /// availability bug above).
    #[test]
    fn gateway_catalog_dedups_live_entries_matching_curated_ids() {
        let Some((dup_id, _label)) = aliases::models_for_source("gateway").into_iter().next()
        else {
            return; // empty catalog — nothing to dedup against
        };
        let live = vec![crate::gateway::client::ModelInfo { id: dup_id.to_string() }];
        let entries = gateway_entries(Some(live));
        let curated_count = aliases::models_for_source("gateway").len();
        assert_eq!(entries.len(), curated_count, "duplicate live id must not add a new row");
    }
}
