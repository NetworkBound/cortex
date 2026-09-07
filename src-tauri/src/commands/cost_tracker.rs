//! Cost tracker — converts the per-session / per-provider token totals
//! already collected by `TracingStore` into dollar estimates using a
//! hard-coded 2026-default pricing table.
//!
//! The tracing store currently only persists a single `total` token count per
//! agent run (no input/output split), so we estimate the split as 50/50 — a
//! reasonable approximation pending a richer token schema. When upstream
//! payloads start emitting `prompt`/`completion` separately, this is the only
//! file that needs to know.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::observability::tracing_store::TracingStore;
// Pricing table + helpers now live in the shared `crate::pricing` module so the
// orchestrator's cost router can reuse them without depending on this command.
use crate::pricing::{compute_usd, lookup_price, split_tokens, DEFAULT_PRICE};

#[derive(Debug, Deserialize)]
pub struct CostEstimateArgs {
    /// When `Some`, the `by_session` array is filtered to just that session.
    /// The `total_usd` field still reflects the filtered total.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CostBySession {
    pub session_id: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub usd: f64,
    pub last_active_ms: i64,
}

#[derive(Debug, Serialize, Clone)]
pub struct CostByModel {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub usd: f64,
    pub input_price_per_million_usd: f64,
    pub output_price_per_million_usd: f64,
}

#[derive(Debug, Serialize, Clone)]
pub struct CostReport {
    pub total_usd: f64,
    pub by_session: Vec<CostBySession>,
    pub by_model: Vec<CostByModel>,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn cost_estimate(
    args: CostEstimateArgs,
    store: State<'_, TracingStore>,
) -> Result<CostReport, String> {
    let by_session_raw = store
        .tokens_by_session(500)
        .map_err(|e| format!("tokens_by_session failed: {e}"))?;
    let by_provider_raw = store
        .tokens_by_provider(100)
        .map_err(|e| format!("tokens_by_provider failed: {e}"))?;

    let by_session: Vec<CostBySession> = by_session_raw
        .into_iter()
        .filter(|s| match &args.session_id {
            Some(want) => &s.session_id == want,
            None => true,
        })
        .map(|s| {
            let (prompt, completion) = split_tokens(s.total_tokens);
            // No per-session model attribution in the store today — fall back
            // to the default price for the session-level rollup. The
            // by_model breakdown below gives more accurate per-model figures.
            let usd = compute_usd(prompt, completion, DEFAULT_PRICE);
            CostBySession {
                session_id: s.session_id,
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: s.total_tokens,
                usd,
                last_active_ms: s.last_active_ms,
            }
        })
        .collect();

    let by_model: Vec<CostByModel> = by_provider_raw
        .into_iter()
        .map(|p| {
            let price = lookup_price(&p.agent_id);
            let (prompt, completion) = split_tokens(p.total_tokens);
            let usd = compute_usd(prompt, completion, price);
            CostByModel {
                model: p.agent_id,
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: p.total_tokens,
                usd,
                input_price_per_million_usd: price.0,
                output_price_per_million_usd: price.1,
            }
        })
        .collect();

    // Prefer the by_model rollup for the headline total — it's priced
    // accurately. Fall back to summing the session rows when there are no
    // provider attributions yet (fresh DB, no completed runs).
    let total_usd = if by_model.is_empty() {
        by_session.iter().map(|s| s.usd).sum()
    } else if args.session_id.is_some() {
        // Filtered to a single session — by_model is a global aggregate so we
        // can't trust it here. Use the per-session row instead.
        by_session.iter().map(|s| s.usd).sum()
    } else {
        by_model.iter().map(|m| m.usd).sum()
    };

    Ok(CostReport {
        total_usd,
        by_session,
        by_model,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

