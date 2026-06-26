//! Tauri commands backing the "ultimate" multi-model agent
//! ([`orchestrator::ultimate`]). The engine is Tauri-free (registry + tracing
//! store in, an `emit` callback out); this file adapts that shape to async Tauri
//! handlers, streams each [`UltEvent`] to the frontend over a single
//! `ultimate:event` channel, and returns the final [`UltimateResult`].
//!
//! Mirrors the wiring in [`crate::commands::teams`]: the registry comes from
//! `AppState`, the [`TracingStore`] from the managed state, and progress is
//! pushed to the UI via `app.emit(...)`.

use crate::app_state::AppState;
use crate::observability::tracing_store::TracingStore;
use crate::orchestrator::ultimate::{self, UltimateConfig, UltimateResult};
use tauri::{AppHandle, Emitter, Manager, State};

/// The window event every streamed [`ultimate::UltEvent`] is forwarded on. The
/// payload is the serialized event itself (a tagged enum — `{ "type": "plan",
/// ... }`, `{ "type": "model_done", ... }`, etc.), so the frontend listens once
/// and switches on `payload.type`.
const ULTIMATE_EVENT: &str = "ultimate:event";

/// Run the ultimate hybrid orchestrator end-to-end for `goal`, streaming
/// progress to the UI over the [`ULTIMATE_EVENT`] channel and returning the
/// synthesized deliverable. `fan_out` defaults to 3 (the engine clamps to ≥1);
/// `lead_model` pins the decomposer, `None` lets the engine pick the strongest
/// capable model. Unlike `run_team` this awaits the whole run (it's a one-shot
/// request/response from the composer), so the result is returned directly.
#[tauri::command]
pub async fn ultimate_chat_run(
    app: AppHandle,
    state: State<'_, AppState>,
    goal: String,
    project_root: Option<String>,
    fan_out: Option<usize>,
    lead_model: Option<String>,
) -> Result<UltimateResult, String> {
    if goal.trim().is_empty() {
        return Err("Give the agent a goal first.".into());
    }
    let cfg = UltimateConfig {
        goal,
        project_root,
        fan_out: fan_out.unwrap_or(3),
        lead_model,
    };

    let registry = state.registry.clone();
    let store = app.state::<TracingStore>().inner().clone();

    // Forward each engine event verbatim to the UI. The closure must be
    // Send + Sync (the engine fans subtasks across tasks); cloning the cheap
    // `AppHandle` into it satisfies that and avoids borrowing `app`.
    let emit_app = app.clone();
    ultimate::run_ultimate(registry, store, cfg, move |ev| {
        let _ = emit_app.emit(ULTIMATE_EVENT, &ev);
    })
    .await
}

/// List the DISTINCT model slugs the ultimate agent could fan out across (live
/// local Ollama tags + every catalog/CLI/API model the registry can reach for a
/// chat task). Drives the lead-model picker in the UI.
#[tauri::command]
pub async fn ultimate_list_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(ultimate::discover_models(&state.registry).await)
}
