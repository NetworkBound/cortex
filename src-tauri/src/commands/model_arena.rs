//! Model Arena — Open WebUI-style side-by-side A/B compare across 2-4 models
//! with a persistent ELO leaderboard at `~/.cortex/arena-elo.json`.
//!
//! Three commands:
//! - `arena_send`: ship one prompt in parallel to N models (2..=4). Each leg is
//!   routed to the same adapter the main composer would use for that model
//!   (`orchestrator::route`) — Claude slugs → the local `claude` CLI, `ollama:`
//!   slugs → the local Ollama daemon, everything else → the Cortex Gateway — so
//!   the Arena can compare *across* adapters, not just across gateway models.
//!   Each transcript is collected into a [`ModelTurn`] (which records the
//!   resolving adapter), and all are returned in one [`ArenaRun`].
//! - `arena_vote`: apply a K=32 ELO update — the winner takes a "pot" against
//!   each loser, then we persist the new ratings + W/L counters to disk.
//! - `arena_leaderboard`: read the JSON store, sorted by rating descending.
//!
//! No new dependencies; the store is plain `serde_json` over a `HashMap` so a
//! corrupted file can be hand-edited if needed.

use crate::agents::{AgentAdapter, AgentEvent, ChatRequest, Registry};
use crate::app_state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

/// Default ELO score for a model the user has never voted on.
const DEFAULT_RATING: f64 = 1200.0;
/// K-factor — standard chess value. With K=32 a model can move ±32 points
/// per duel against an equal-rated opponent.
const K_FACTOR: f64 = 32.0;
/// Hard ceiling on the number of models per arena run. Keeps the gateway from
/// getting hammered when the user clicks every chip on a model-heavy gateway.
const MAX_MODELS: usize = 4;
/// 60s wall clock for each leg of the arena. Each model streams independently
/// so this is a per-model timeout, not a per-run one.
const PER_MODEL_TIMEOUT: Duration = Duration::from_secs(60);

// ---------- Wire types ----------

/// One model's transcript for a single arena prompt. `error` is `None` when
/// the call succeeded; any non-empty `error` means `response` is best-effort
/// (partial output, possibly empty) and the run should not be vote-eligible.
#[derive(Debug, Clone, Serialize)]
pub struct ModelTurn {
    pub model: String,
    /// The adapter that actually served this leg (`claude-cli` / `ollama` /
    /// `gateway-remote`). Empty when no adapter could be resolved. Lets the UI
    /// show *where* each answer came from in a cross-adapter comparison.
    pub adapter: String,
    pub response: String,
    pub tokens: u64,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// The aggregate result of one /arena send across N models.
#[derive(Debug, Clone, Serialize)]
pub struct ArenaRun {
    pub run_id: String,
    pub models: Vec<ModelTurn>,
}

/// On-disk row for a single model's ELO + W/L history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRating {
    pub model: String,
    pub rating: f64,
    pub wins: u64,
    pub losses: u64,
    pub total_runs: u64,
}

/// What the frontend gets back from `arena_vote` — the full updated table,
/// not just deltas, so the leaderboard sidebar can render without a second
/// roundtrip.
#[derive(Debug, Clone, Serialize)]
pub struct EloUpdate {
    pub ratings: Vec<ModelRating>,
}

/// Raw disk format. `HashMap<model_id, ModelRating>` keeps lookups O(1) and
/// avoids us having to maintain a separate sort index — `arena_leaderboard`
/// sorts on read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EloStore {
    #[serde(default)]
    ratings: HashMap<String, ModelRating>,
}

// ---------- Commands ----------

#[tauri::command]
pub async fn arena_send(
    prompt: String,
    models: Vec<String>,
    state: State<'_, AppState>,
) -> Result<ArenaRun, String> {
    if prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    if models.len() < 2 {
        return Err("arena needs at least 2 models".into());
    }
    if models.len() > MAX_MODELS {
        return Err(format!("arena capped at {MAX_MODELS} models"));
    }

    // De-dupe + drop blanks while preserving order. A user double-clicking
    // the same chip shouldn't burn two API calls.
    let mut seen = std::collections::HashSet::new();
    let cleaned: Vec<String> = models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && seen.insert(m.clone()))
        .collect();
    if cleaned.len() < 2 {
        return Err("need at least 2 distinct models".into());
    }

    let run_ts = chrono::Utc::now().timestamp_millis();

    // Resolve each model to its adapter using the SAME routing the composer
    // uses (`orchestrator::route`), then grab the registered adapter instance.
    // Done under one short read-lock; the `Arc<dyn AgentAdapter>` clones are
    // cheap and outlive the guard so no lock is held across an `.await`.
    let resolved: Vec<(String, String, Option<Arc<dyn AgentAdapter>>)> = {
        let registry = state.registry.read();
        cleaned
            .iter()
            .map(|m| match resolve_adapter_for_model(m, &registry) {
                Some((id, adapter)) => (m.clone(), id, Some(adapter)),
                None => (m.clone(), String::new(), None),
            })
            .collect()
    };

    // Spawn one task per leg so all calls run in parallel — no JoinSet to avoid
    // pulling tokio_util in. We track index so we can resync order after the
    // unordered join.
    let mut handles = Vec::with_capacity(resolved.len());
    for (idx, (model, adapter_id, adapter)) in resolved.into_iter().enumerate() {
        let prompt_clone = prompt.clone();
        let session_id = format!("arena-{run_ts}-{idx}");
        let h = tokio::spawn(async move {
            let turn = match adapter {
                Some(ad) => run_one_model(ad, adapter_id, model, prompt_clone, session_id).await,
                None => ModelTurn {
                    model,
                    adapter: String::new(),
                    response: String::new(),
                    tokens: 0,
                    latency_ms: 0,
                    error: Some("no adapter is available to serve this model".into()),
                },
            };
            (idx, turn)
        });
        handles.push(h);
    }

    // Preserve the original model order so column N in the UI always maps to
    // the Nth chip the user selected. JoinError → mark the slot as failed.
    let mut slots: Vec<Option<ModelTurn>> = vec![None; cleaned.len()];
    for h in handles {
        match h.await {
            Ok((idx, turn)) => slots[idx] = Some(turn),
            Err(e) => {
                // We don't know which slot panicked — find the first empty
                // slot whose model we haven't filled and stamp it as error.
                let fallback_idx = slots.iter().position(|s| s.is_none()).unwrap_or(0);
                let model_id = cleaned.get(fallback_idx).cloned().unwrap_or_default();
                slots[fallback_idx] = Some(ModelTurn {
                    model: model_id,
                    adapter: String::new(),
                    response: String::new(),
                    tokens: 0,
                    latency_ms: 0,
                    error: Some(format!("task join error: {e}")),
                });
            }
        }
    }

    let turns: Vec<ModelTurn> = slots
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            slot.unwrap_or_else(|| ModelTurn {
                model: cleaned[i].clone(),
                adapter: String::new(),
                response: String::new(),
                tokens: 0,
                latency_ms: 0,
                error: Some("unknown failure".into()),
            })
        })
        .collect();

    Ok(ArenaRun {
        run_id: format!("arena-{run_ts}"),
        models: turns,
    })
}

#[tauri::command]
pub async fn arena_vote(
    run_id: String,
    winner: String,
    losers: Vec<String>,
) -> Result<EloUpdate, String> {
    let _ = run_id; // accepted for symmetry / future telemetry — not persisted today
    let winner = winner.trim().to_string();
    if winner.is_empty() {
        return Err("winner is empty".into());
    }
    // De-duplicate losers (preserving first-seen order) so a repeated id is
    // only penalized once and the winner is only credited a single win per
    // distinct opponent in this duel.
    let mut seen = std::collections::HashSet::new();
    let losers: Vec<String> = losers
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != &winner)
        .filter(|s| seen.insert(s.clone()))
        .collect();
    if losers.is_empty() {
        return Err("need at least 1 loser".into());
    }

    let path = elo_path()?;
    let mut store = read_store(&path);

    // Apply a separate K=32 update for each (winner, loser) pair. This is
    // equivalent to the standard "round-robin against the field" approach
    // and keeps the math symmetric: total points are conserved across each
    // pair, so a 3-way duel never inflates the rating economy.
    {
        // Take owned snapshots of the two ratings, mutate, then write back.
        // Two separate scopes avoid double-borrowing the HashMap.
        for loser in &losers {
            let winner_rating = store
                .ratings
                .get(&winner)
                .map(|r| r.rating)
                .unwrap_or(DEFAULT_RATING);
            let loser_rating = store
                .ratings
                .get(loser)
                .map(|r| r.rating)
                .unwrap_or(DEFAULT_RATING);
            let (new_winner, new_loser) = elo_update(winner_rating, loser_rating);

            let w = store
                .ratings
                .entry(winner.clone())
                .or_insert_with(|| ModelRating::new(&winner));
            w.rating = new_winner;
            w.wins += 1;
            w.total_runs += 1;

            let l = store
                .ratings
                .entry(loser.clone())
                .or_insert_with(|| ModelRating::new(loser));
            l.rating = new_loser;
            l.losses += 1;
            l.total_runs += 1;
        }
    }

    write_store(&path, &store)?;
    Ok(EloUpdate {
        ratings: sorted_ratings(&store),
    })
}

#[tauri::command]
pub async fn arena_leaderboard() -> Result<Vec<ModelRating>, String> {
    let path = elo_path()?;
    let store = read_store(&path);
    Ok(sorted_ratings(&store))
}

// ---------- Helpers ----------

impl ModelRating {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            rating: DEFAULT_RATING,
            wins: 0,
            losses: 0,
            total_runs: 0,
        }
    }
}

/// Standard ELO update for a winner against a loser.
///
/// `expected = 1 / (1 + 10^((opponent - self) / 400))`, then
/// `new = self + K * (actual - expected)`.
fn elo_update(winner: f64, loser: f64) -> (f64, f64) {
    let expected_winner = 1.0 / (1.0 + 10f64.powf((loser - winner) / 400.0));
    let expected_loser = 1.0 - expected_winner;
    let new_winner = winner + K_FACTOR * (1.0 - expected_winner);
    let new_loser = loser + K_FACTOR * (0.0 - expected_loser);
    (new_winner, new_loser)
}

fn sorted_ratings(store: &EloStore) -> Vec<ModelRating> {
    let mut out: Vec<ModelRating> = store.ratings.values().cloned().collect();
    out.sort_by(|a, b| {
        b.rating
            .partial_cmp(&a.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn elo_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "could not resolve home dir".to_string())?;
    let dir = home.join(".cortex");
    fs::create_dir_all(&dir).map_err(|e| format!("create ~/.cortex failed: {e}"))?;
    Ok(dir.join("arena-elo.json"))
}

fn read_store(path: &PathBuf) -> EloStore {
    let Ok(raw) = fs::read_to_string(path) else {
        return EloStore::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_store(path: &PathBuf, store: &EloStore) -> Result<(), String> {
    let json = serde_json::to_string_pretty(store)
        .map_err(|e| format!("serialise elo store failed: {e}"))?;
    fs::write(path, json).map_err(|e| format!("write {} failed: {e}", path.display()))
}

/// Resolve a model id to the adapter that should serve it, using the *same*
/// `orchestrator::route` the main composer uses. Returns the resolved adapter
/// id plus the registered adapter instance, or `None` when no available adapter
/// matches (e.g. an empty registry). Claude slugs → `claude-cli`, `ollama:`
/// slugs → `ollama`, everything else → `gateway-remote` (or the first available
/// adapter as a last resort).
fn resolve_adapter_for_model(
    model: &str,
    registry: &Registry,
) -> Option<(String, Arc<dyn AgentAdapter>)> {
    let req = ChatRequest {
        session_id: String::new(),
        message: String::new(),
        project_root: None,
        history: Vec::new(),
        model: Some(model.to_string()),
        reasoning_effort: None,
    };
    let decision = crate::orchestrator::route(&req, registry, None);
    let id = decision.agents.into_iter().next()?;
    let adapter = registry.get(&id)?;
    Some((id, adapter))
}

/// Drive one arena leg through its resolved adapter, with a per-model timeout.
/// Errors (timeout, transport failure, adapter error) come back inside the
/// returned [`ModelTurn`] as `error: Some(...)` rather than a top-level `Err` —
/// so a single failing column never aborts the others.
async fn run_one_model(
    adapter: Arc<dyn AgentAdapter>,
    adapter_id: String,
    model: String,
    prompt: String,
    session_id: String,
) -> ModelTurn {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);

    let req = ChatRequest {
        session_id,
        message: prompt,
        project_root: None,
        history: Vec::new(),
        model: Some(model.clone()),
        reasoning_effort: None,
    };

    let run_fut = {
        let adapter = adapter.clone();
        async move { adapter.run(req, tx).await }
    };

    // Drain every event until the channel closes (the adapter drops `tx` when
    // `run` returns). Concatenate token deltas, capture the first error, and
    // take the token total off `Done` when the adapter reports one.
    let collect_fut = async {
        let mut body = String::new();
        let mut tokens = 0u64;
        let mut error: Option<String> = None;
        while let Some(evt) = rx.recv().await {
            match evt {
                AgentEvent::Token { delta } => body.push_str(&delta),
                AgentEvent::Error { message } => {
                    if error.is_none() {
                        error = Some(message);
                    }
                }
                AgentEvent::Done { total_tokens, .. } => {
                    if let Some(t) = total_tokens {
                        tokens = t;
                    }
                }
                _ => {}
            }
        }
        (body, tokens, error)
    };

    let result = tokio::time::timeout(PER_MODEL_TIMEOUT, async {
        let (run_res, collected) = tokio::join!(run_fut, collect_fut);
        (run_res, collected)
    })
    .await;

    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok((run_res, (body, tokens, mut error))) => {
            // A top-level adapter error that wasn't already surfaced as an
            // `AgentEvent::Error` still marks the leg failed.
            if error.is_none() {
                if let Err(e) = run_res {
                    error = Some(format!("adapter error: {e}"));
                }
            }
            ModelTurn {
                model,
                adapter: adapter_id,
                response: body,
                tokens,
                latency_ms,
                error,
            }
        }
        Err(_) => ModelTurn {
            model,
            adapter: adapter_id,
            response: String::new(),
            tokens: 0,
            latency_ms,
            error: Some(format!("timed out after {}s", PER_MODEL_TIMEOUT.as_secs())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::{AgentCapability, AgentDescriptor};
    use async_trait::async_trait;

    /// A no-network adapter that echoes a known body + token count, so the
    /// cross-adapter routing and event-collection can be asserted offline.
    struct StubAdapter {
        id: String,
        available: bool,
    }

    #[async_trait]
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.id.clone(),
                label: self.id.clone(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: self.available,
            }
        }
        async fn health_check(&self) -> bool {
            self.available
        }
        async fn run(
            &self,
            req: ChatRequest,
            tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            // Echo the resolved model back so the test can prove the per-call
            // model override reached the adapter.
            let model = req.model.unwrap_or_default();
            let _ = tx
                .send(AgentEvent::Token {
                    delta: format!("[{}] {model}", self.id),
                })
                .await;
            let _ = tx
                .send(AgentEvent::Done {
                    total_tokens: Some(11),
                    run_id: None,
                })
                .await;
            Ok(())
        }
    }

    fn full_registry() -> Registry {
        let mut reg = Registry::new();
        reg.register(Arc::new(StubAdapter {
            id: "claude-cli".into(),
            available: true,
        }));
        reg.register(Arc::new(StubAdapter {
            id: "ollama".into(),
            available: true,
        }));
        reg.register(Arc::new(StubAdapter {
            id: "gateway-remote".into(),
            available: true,
        }));
        reg
    }

    #[test]
    fn resolve_routes_claude_slug_to_claude_cli() {
        let reg = full_registry();
        for slug in ["claude-sonnet-4-6", "opus-4.8", "sonnet", "haiku-4-5"] {
            let (id, _) = resolve_adapter_for_model(slug, &reg)
                .unwrap_or_else(|| panic!("no adapter for {slug}"));
            assert_eq!(id, "claude-cli", "{slug} should route to claude-cli");
        }
    }

    #[test]
    fn resolve_routes_ollama_slug_to_ollama() {
        let reg = full_registry();
        for slug in ["ollama:llama3", "ollama/qwen2.5-coder"] {
            let (id, _) = resolve_adapter_for_model(slug, &reg)
                .unwrap_or_else(|| panic!("no adapter for {slug}"));
            assert_eq!(id, "ollama", "{slug} should route to ollama");
        }
    }

    #[test]
    fn resolve_routes_other_slugs_to_gateway_default() {
        let reg = full_registry();
        for slug in ["gpt-5.5", "gemini-2.5-pro", "some-gateway-model"] {
            let (id, _) = resolve_adapter_for_model(slug, &reg)
                .unwrap_or_else(|| panic!("no adapter for {slug}"));
            assert_eq!(id, "gateway-remote", "{slug} should route to gateway default");
        }
    }

    #[test]
    fn resolve_falls_back_when_claude_cli_absent() {
        // No claude-cli registered → a Claude slug must NOT error; it falls
        // through to the gateway default so the leg still runs.
        let mut reg = Registry::new();
        reg.register(Arc::new(StubAdapter {
            id: "gateway-remote".into(),
            available: true,
        }));
        let (id, _) = resolve_adapter_for_model("claude-opus-4-8", &reg).unwrap();
        assert_eq!(id, "gateway-remote");
    }

    #[test]
    fn resolve_empty_registry_yields_none() {
        let reg = Registry::new();
        assert!(resolve_adapter_for_model("claude-sonnet-4-6", &reg).is_none());
    }

    #[tokio::test]
    async fn run_one_model_collects_body_tokens_and_adapter() {
        let adapter: Arc<dyn AgentAdapter> = Arc::new(StubAdapter {
            id: "claude-cli".into(),
            available: true,
        });
        let turn = run_one_model(
            adapter,
            "claude-cli".into(),
            "claude-sonnet-4-6".into(),
            "hello".into(),
            "arena-test-0".into(),
        )
        .await;
        assert_eq!(turn.adapter, "claude-cli");
        assert_eq!(turn.model, "claude-sonnet-4-6");
        // The stub echoes its id + the resolved model, proving the per-call
        // model override flowed through to the adapter.
        assert_eq!(turn.response, "[claude-cli] claude-sonnet-4-6");
        assert_eq!(turn.tokens, 11);
        assert!(turn.error.is_none());
    }

    #[test]
    fn elo_update_conserves_points() {
        let (w, l) = elo_update(1200.0, 1200.0);
        // Equal-rated duel → winner gains, loser loses, exactly K/2 each way.
        assert!((w - 1216.0).abs() < 0.01);
        assert!((l - 1184.0).abs() < 0.01);
        // Total points conserved.
        assert!(((w + l) - 2400.0).abs() < 0.01);
    }

    #[test]
    fn elo_update_underdog_win_swings_more() {
        let (w, _) = elo_update(1000.0, 1400.0);
        let (w2, _) = elo_update(1400.0, 1000.0);
        // Underdog (1000) beating 1400 gains more than favorite (1400) beating 1000.
        let underdog_delta = w - 1000.0;
        let favorite_delta = w2 - 1400.0;
        assert!(underdog_delta > favorite_delta);
    }

    #[test]
    fn sorted_ratings_descending() {
        let mut store = EloStore::default();
        store.ratings.insert(
            "a".into(),
            ModelRating { model: "a".into(), rating: 1100.0, wins: 0, losses: 1, total_runs: 1 },
        );
        store.ratings.insert(
            "b".into(),
            ModelRating { model: "b".into(), rating: 1300.0, wins: 1, losses: 0, total_runs: 1 },
        );
        let out = sorted_ratings(&store);
        assert_eq!(out[0].model, "b");
        assert_eq!(out[1].model, "a");
    }
}
