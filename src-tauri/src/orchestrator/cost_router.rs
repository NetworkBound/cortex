//! Cost-aware, capability-aware model picker.
//!
//! Given a task's *difficulty* and the *capabilities* it requires, choose a
//! concrete model slug to dispatch — the cheapest model that can do the job for
//! easy work, the strongest for hard work — drawing only from models the
//! current [`Registry`] can actually reach.
//!
//! This is the shared primitive for the orchestration slices: Teams will tag
//! each planned subtask `easy|hard` + `chat|code` (slice 2) and route it
//! through [`pick_model_for`] when the role pins no explicit model (slice 3).
//!
//! ## How candidates are assembled
//!
//! - **Cloud / local-CLI models** come from the curated
//!   [`aliases::CATALOG`](crate::orchestrator::aliases). Each catalog entry
//!   names the adapter that serves it (`claude-cli` for the local Claude Code
//!   CLI, `gateway` → the `gateway-remote` adapter). A catalog model is a
//!   candidate only when its serving adapter is **registered, available, and
//!   advertises every required capability**. Each is priced via
//!   [`crate::pricing::lookup_price`].
//! - **Local Ollama models** are passed in by the caller (`local_models`, the
//!   live `ollama:<tag>` slugs the model picker already discovers) because tags
//!   are discovered at runtime, not curated. They are free (`$0`) and inherit
//!   the Ollama adapter's capabilities — so a repo-editing task that needs
//!   `ShellExec` correctly skips them (this app's Ollama adapter is chat/edit
//!   only, no shell).
//!
//! Because chat-only direct adapters (`anthropic_direct` / `openai_direct`) are
//! deliberately absent from the catalog and never advertise `ShellExec`, a
//! `ShellExec`-requiring task can never be routed to one — the core safety
//! property the orchestration slices depend on.

use crate::agents::adapter::AgentCapability;
use crate::agents::registry::Registry;
use crate::observability::tracing_store::ReliabilityRow;
use crate::orchestrator::aliases::CATALOG;
use crate::pricing::lookup_price;
use serde::{Deserialize, Serialize};

/// Coarse task difficulty. Drives the cheap-vs-strong tradeoff; the capability
/// filter is orthogonal (passed separately as `required_caps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    /// Routine work — pick the cheapest capable model (a local Ollama tag or a
    /// mini/flash cloud model).
    Easy,
    /// Demanding work — pick the strongest (most expensive) capable model.
    Hard,
}

/// A concrete, dispatchable model choice plus the metadata callers need to
/// route it and to project its cost.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPick {
    /// The model slug to dispatch, exactly as the composer's picker produces it
    /// (`claude-opus-4-8`, `gpt-4o-mini`, `ollama:llama3.2:1b`).
    pub model: String,
    /// Registry id of the adapter that serves it (`claude-cli`, `gateway-remote`,
    /// `ollama`).
    pub agent_id: String,
    /// `$ / 1M` input price (0 for local models).
    pub input_price_per_million_usd: f64,
    /// `$ / 1M` output price (0 for local models).
    pub output_price_per_million_usd: f64,
    /// `true` when this is a free local model (Ollama).
    pub local: bool,
    /// Human-readable "why this model" — filled in by [`pick_model_for`]
    /// on the winning pick (`""` on raw [`candidates`] entries). Surfaces the
    /// cheapest-capable / local-preferred / strongest-required framing in
    /// team-run traces instead of an unexplained slug.
    pub reason: String,
}

impl ModelPick {
    /// Combined input+output price — the scalar used to rank cheap vs strong.
    fn price_sum(&self) -> f64 {
        self.input_price_per_million_usd + self.output_price_per_million_usd
    }
}

/// Map a catalog `source` to the registry adapter id that serves it. The
/// catalog uses the short `"gateway"` label; the registry id is `"gateway-remote"`.
fn source_to_registry_id(source: &str) -> &str {
    match source {
        "gateway" => "gateway-remote",
        other => other,
    }
}

/// Does `caps` satisfy every entry in `required`?
fn satisfies(caps: &[AgentCapability], required: &[AgentCapability]) -> bool {
    required.iter().all(|r| caps.contains(r))
}

/// Build the full candidate set for `required_caps` against the live registry.
///
/// Exposed (`pub`) so multi-model fan-out callers (the "ultimate" orchestrator)
/// can enumerate the whole capable-model roster, not just the single cheapest /
/// strongest pick [`pick_model_for`] returns.
pub fn candidates(
    required_caps: &[AgentCapability],
    registry: &Registry,
    local_models: &[String],
) -> Vec<ModelPick> {
    let descriptors = registry.list_descriptors();
    let mut out: Vec<ModelPick> = Vec::new();

    // Curated cloud / CLI models from the catalog. Prefer the model's local
    // `cli_source` adapter when it's registered, available, AND capable — this
    // is the "default-to-local" behavior. Fall back to the gateway `source`
    // when the CLI isn't installed (or can't do the required work), so an
    // absent CLI never strands the model.
    for m in CATALOG {
        let mut chosen: Option<&str> = None;
        if let Some(cli) = m.cli_source {
            if let Some(desc) = descriptors.iter().find(|d| d.id == cli) {
                if desc.available && satisfies(&desc.capabilities, required_caps) {
                    chosen = Some(cli);
                }
            }
        }
        if chosen.is_none() {
            let reg_id = source_to_registry_id(m.source);
            if let Some(desc) = descriptors.iter().find(|d| d.id == reg_id) {
                if desc.available && satisfies(&desc.capabilities, required_caps) {
                    chosen = Some(reg_id);
                }
            }
        }
        let Some(reg_id) = chosen else { continue };
        let (inp, outp) = lookup_price(m.id);
        out.push(ModelPick {
            model: m.id.to_string(),
            agent_id: reg_id.to_string(),
            input_price_per_million_usd: inp,
            output_price_per_million_usd: outp,
            local: false,
            reason: String::new(),
        });
    }

    // Live local Ollama tags supplied by the caller. Free, and only viable when
    // the Ollama adapter is available and capable of the required work.
    if !local_models.is_empty() {
        if let Some(desc) = descriptors.iter().find(|d| d.id == "ollama") {
            if desc.available && satisfies(&desc.capabilities, required_caps) {
                for slug in local_models {
                    let slug = slug.trim();
                    if slug.is_empty() {
                        continue;
                    }
                    out.push(ModelPick {
                        model: slug.to_string(),
                        agent_id: "ollama".to_string(),
                        input_price_per_million_usd: 0.0,
                        output_price_per_million_usd: 0.0,
                        local: true,
                        reason: String::new(),
                    });
                }
            }
        }
    }

    out
}

/// Pick the best model for a task of `difficulty` that needs `required_caps`,
/// drawing only from models the `registry` can reach (plus the caller-supplied
/// live `local_models` Ollama tags). Returns `None` when nothing available can
/// satisfy the capability requirements.
///
/// - `Easy`  → the cheapest capable model (local Ollama wins on price; ties
///   break toward the local model, then the cheaper input price).
/// - `Hard`  → the strongest (most expensive) capable model; ties break toward
///   the more expensive input price and away from free local models.
pub fn pick_model_for(
    difficulty: Difficulty,
    required_caps: &[AgentCapability],
    registry: &Registry,
    local_models: &[String],
) -> Option<ModelPick> {
    let cands = candidates(required_caps, registry, local_models);
    if cands.is_empty() {
        return None;
    }
    let n = cands.len();
    let mut pick = match difficulty {
        Difficulty::Easy => cands.into_iter().min_by(|a, b| {
            a.price_sum()
                .partial_cmp(&b.price_sum())
                .unwrap_or(std::cmp::Ordering::Equal)
                // Tie: prefer the free local model, then the lower input price.
                .then(b.local.cmp(&a.local))
                .then(
                    a.input_price_per_million_usd
                        .partial_cmp(&b.input_price_per_million_usd)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        }),
        Difficulty::Hard => cands.into_iter().max_by(|a, b| {
            a.price_sum()
                .partial_cmp(&b.price_sum())
                .unwrap_or(std::cmp::Ordering::Equal)
                // Tie: prefer the non-local model, then the higher input price.
                .then(a.local.cmp(&b.local))
                .then(
                    a.input_price_per_million_usd
                        .partial_cmp(&b.input_price_per_million_usd)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        }),
    }?;
    pick.reason = match difficulty {
        Difficulty::Easy if pick.local => {
            format!("routine task → free local model preferred ({n} candidates)")
        }
        Difficulty::Easy => format!(
            "routine task → cheapest capable model at ${:.2}/M combined ({n} candidates)",
            pick.price_sum()
        ),
        Difficulty::Hard => format!(
            "demanding task → strongest capable model at ${:.2}/M combined ({n} candidates)",
            pick.price_sum()
        ),
    };
    Some(pick)
}

// ─────────────── Cost-per-success (outcome-aware) routing — issue 006 ────────
//
// OPT-IN and DEFAULT-OFF. Feeds the Reliability aggregates (issue 002 — a
// LOCAL VIEW derived from `agent.run` spans, see `tracing_store.rs`) back into
// routing as a *hint* for `orchestrator::route_with_outcome`'s DEFAULT branch
// only: explicit picks, model routes, and the CLI/safety branches never see
// it. Scoring is `success_rate / est_cost_per_run`, gated by a minimum run
// count and a recency window — when the data is too thin, no hint is produced
// and routing is byte-identical to today. Reads only local data; no network.

/// Minimum finished runs (ok + error) a provider needs inside the recency
/// window before its outcomes may steer routing. Below this the sample is
/// noise and the router must fall back to today's exact behavior.
pub const OUTCOME_MIN_RUNS: u64 = 5;

/// Recency window for outcome data: 7 days. Runs older than this say little
/// about how a provider behaves *now* (models get swapped, endpoints move).
pub const OUTCOME_RECENCY_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Floor for the estimated cost of one run, in USD. Free local providers
/// report `$0` spend; dividing by this floor instead keeps their score finite
/// while still letting a reliable free provider win on cost — the intent.
const OUTCOME_COST_FLOOR_USD: f64 = 0.0001;

/// Aggregated recent outcomes for one provider (registry adapter id) — the
/// pure input to [`outcome_score`]. Mapped from the Reliability dashboard's
/// per-provider rows by [`outcome_stats_from_reliability`].
#[derive(Debug, Clone, PartialEq)]
pub struct OutcomeStats {
    /// Registry adapter id (`gateway-remote`, `claude-cli`, `ollama`, …).
    pub key: String,
    /// Finished runs in the window: ok + error (running/stale excluded).
    pub finished_runs: u64,
    /// `ok / (ok + error)` over the window, `0.0` when nothing finished.
    pub success_rate: f64,
    /// Estimated total spend over the window (local pricing heuristic).
    pub est_usd: f64,
    /// Start timestamp (epoch ms) of the most recent run in the group.
    pub last_run_ms: i64,
}

impl OutcomeStats {
    /// Estimated `$ / run`, floored so free providers stay finite.
    fn est_cost_per_run(&self) -> f64 {
        if self.finished_runs == 0 {
            return OUTCOME_COST_FLOOR_USD;
        }
        (self.est_usd / self.finished_runs as f64).max(OUTCOME_COST_FLOOR_USD)
    }
}

/// The winning outcome-routing hint plus the human-readable rationale that
/// surfaces through the existing `routing_reason` plumbing.
#[derive(Debug, Clone, PartialEq)]
pub struct OutcomePick {
    /// Registry adapter id to prefer in the default branch.
    pub agent_id: String,
    /// The provider's [`outcome_score`], kept for traces/tests.
    pub score: f64,
    /// "Why this provider" — e.g.
    /// `outcome-route → ollama: 96% success over 25 runs at ~$0.0001/run (7d)`.
    pub reason: String,
}

/// Score one provider's recent outcomes: `success_rate / est_cost_per_run`.
///
/// Returns `None` — "data too thin, don't let this steer routing" — when the
/// provider has fewer than [`OUTCOME_MIN_RUNS`] finished runs or its most
/// recent run is older than [`OUTCOME_RECENCY_WINDOW_MS`]. Pure.
pub fn outcome_score(stats: &OutcomeStats, now_ms: i64) -> Option<f64> {
    outcome_score_with_exponent(stats, now_ms, 1.0)
}

/// [`outcome_score`] generalized with a cost exponent: `success_rate /
/// est_cost_per_run.powf(cost_exponent)`. `cost_exponent == 1.0` reproduces
/// `outcome_score` exactly (same gates, same arithmetic — `1.0` is
/// special-cased below rather than routed through `powf` so the two are
/// bit-for-bit identical, not just numerically close). A higher exponent
/// weights cost more heavily than raw success-rate-per-dollar, which is how
/// [`pick_agent_by_outcome_with_budget`] biases toward cheaper providers once
/// a session's spend is approaching its cap. Pure.
fn outcome_score_with_exponent(stats: &OutcomeStats, now_ms: i64, cost_exponent: f64) -> Option<f64> {
    if stats.finished_runs < OUTCOME_MIN_RUNS {
        return None;
    }
    if now_ms.saturating_sub(stats.last_run_ms) > OUTCOME_RECENCY_WINDOW_MS {
        return None;
    }
    let cost = stats.est_cost_per_run();
    let denom = if cost_exponent == 1.0 { cost } else { cost.powf(cost_exponent) };
    Some(stats.success_rate.clamp(0.0, 1.0) / denom)
}

/// Map the Reliability dashboard's per-provider rows (`by_provider` from
/// `TracingStore::reliability_summary` / `provider_reliability`) into the pure
/// [`OutcomeStats`] scoring inputs.
pub fn outcome_stats_from_reliability(rows: &[ReliabilityRow]) -> Vec<OutcomeStats> {
    rows.iter()
        .map(|r| OutcomeStats {
            key: r.key.clone(),
            finished_runs: r.ok_runs + r.error_runs,
            success_rate: r.success_rate,
            est_usd: r.est_usd,
            last_run_ms: r.last_run_ms,
        })
        .collect()
}

/// Pick the provider with the best recent success-rate-per-dollar, drawing
/// only from providers that are **registered, available, and advertise
/// `Chat`** in the live `registry` (the default branch routes chat work — a
/// provider the registry can't reach, or one that can't chat, is never a
/// candidate no matter how good its history looks).
///
/// Returns `None` — the caller falls back to today's exact behavior — when no
/// eligible provider passes [`outcome_score`]'s min-runs/recency gates. Ties
/// break toward the higher success rate, then the lexicographically smaller
/// id, so the pick is deterministic.
pub fn pick_agent_by_outcome(
    stats: &[OutcomeStats],
    registry: &Registry,
    now_ms: i64,
) -> Option<OutcomePick> {
    pick_best(stats, registry, now_ms, 1.0, false)
}

/// Shared scoring loop behind [`pick_agent_by_outcome`] and
/// [`pick_agent_by_outcome_with_budget`]. `cost_exponent == 1.0` and
/// `budget_biased == false` (what [`pick_agent_by_outcome`] always passes)
/// reproduces its exact eligibility filter, gates, tie-breaks, and reason
/// text — the two are the same code path, not just similar ones.
fn pick_best(
    stats: &[OutcomeStats],
    registry: &Registry,
    now_ms: i64,
    cost_exponent: f64,
    budget_biased: bool,
) -> Option<OutcomePick> {
    let descriptors = registry.list_descriptors();
    let mut best: Option<(f64, &OutcomeStats)> = None;
    for s in stats {
        let eligible = descriptors.iter().any(|d| {
            d.id == s.key && d.available && d.capabilities.contains(&AgentCapability::Chat)
        });
        if !eligible {
            continue;
        }
        let Some(score) = outcome_score_with_exponent(s, now_ms, cost_exponent) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((best_score, best_stats)) => {
                score > *best_score
                    || (score == *best_score
                        && (s.success_rate > best_stats.success_rate
                            || (s.success_rate == best_stats.success_rate
                                && s.key < best_stats.key)))
            }
        };
        if better {
            best = Some((score, s));
        }
    }
    best.map(|(score, s)| OutcomePick {
        agent_id: s.key.clone(),
        score,
        reason: format!(
            "outcome-route → {}: {:.0}% success over {} runs at ~${:.4}/run ({}d window){}",
            s.key,
            s.success_rate.clamp(0.0, 1.0) * 100.0,
            s.finished_runs,
            s.est_cost_per_run(),
            OUTCOME_RECENCY_WINDOW_MS / 86_400_000,
            if budget_biased {
                " — budget: preferring cheaper reliable provider"
            } else {
                ""
            },
        ),
    })
}

/// On-disk schema for `~/.cortex/outcome-routing.json` — the opt-in toggle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutcomeRouting {
    pub enabled: bool,
}

/// `~/.cortex/outcome-routing.json`.
pub fn outcome_routing_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("outcome-routing.json"))
}

/// Parse the toggle file body. Malformed JSON resolves to DEFAULT-OFF —
/// unlike Safe Mode's fail-closed, this is a routing *preference*, and the
/// only safe failure mode for it is "behave exactly like today". Pure.
pub(crate) fn parse_outcome_routing(raw: &str) -> OutcomeRouting {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Cheap per-call check read by `chat_send` on every turn (mirrors
/// `safe_mode::is_enabled`), so the Settings toggle applies without a
/// restart. Missing file — the default install state — is OFF.
pub fn outcome_routing_enabled() -> bool {
    let Some(path) = outcome_routing_path() else {
        return false;
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_outcome_routing(&raw).enabled,
        Err(_) => false,
    }
}

/// Persist the toggle, creating `~/.cortex/` if needed.
pub fn write_outcome_routing(enabled: bool) -> anyhow::Result<()> {
    let path = outcome_routing_path()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&OutcomeRouting { enabled })?;
    std::fs::write(&path, body)?;
    Ok(())
}

// ─────────────── Budget ceilings per session — issue 006 full scope ──────
//
// An OPTIONAL, per-session USD spend cap layered on top of outcome-aware
// routing. A session with no cap (`cap_usd: None`, the default — no file on
// disk) is untouched: `pick_agent_by_outcome_with_budget(.., None)` is the
// exact same code path as `pick_agent_by_outcome` (see `pick_best` above),
// and nothing here ever runs unless a cap has actually been set. Like the
// outcome hint itself, the budget is consulted ONLY by the chat.rs call site
// that feeds the DEFAULT branch's hint — never for an explicit agent pick or
// a model-carrying request (see `commands::chat::chat_send`'s bare-request
// gate, which the budget check reuses verbatim).
//
// Two effects, both gated on `budget_state`:
//   - `Approaching` (spend ≥ `BUDGET_APPROACHING_FRACTION` of the cap): the
//     outcome-routing pick weights cost more heavily (`BUDGET_COST_BIAS_EXPONENT`),
//     so a cheaper reliable provider can outrank a pricier one that would
//     otherwise win on raw success-rate-per-dollar. Still a `success_rate`-
//     gated score — a provider with a terrible success rate scores near zero
//     no matter how cheap, so "cheaper" never means "instead of reliable".
//   - `Exceeded` (spend ≥ the cap): the chat.rs call site blocks the send
//     entirely with an error (before any routing happens) rather than letting
//     it through at a biased pick. Enforced outside `pick_best`/`route` on
//     purpose — a budget is a spending guard, not a routing preference, and
//     must not need touching the routing functions to say "no further spend".

/// A session's optional spend cap plus what it has spent so far. `cap_usd:
/// None` — the default, and the only state that exists before a user sets a
/// cap — means "no cap"; routing and sending must behave exactly like today.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionBudget {
    pub cap_usd: Option<f64>,
    pub spent_usd: f64,
}

/// Once spend reaches this fraction of the cap, outcome-aware routing starts
/// preferring cheaper reliable providers (but does not yet block sending).
pub const BUDGET_APPROACHING_FRACTION: f64 = 0.8;

/// Cost exponent applied once a budget is `Approaching`/`Exceeded`: squaring
/// the cost term (vs. the plain `/ cost` in [`outcome_score`]) weights $/run
/// much more heavily, so the cheaper of two reliable providers wins even when
/// its success rate is somewhat lower.
const BUDGET_COST_BIAS_EXPONENT: f64 = 2.0;

/// Where a session's spend sits relative to its cap. Pure classification —
/// no I/O. `Unlimited` (no cap set) is the default and behaves like today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    /// No cap set for this session — today's exact behavior.
    Unlimited,
    /// A cap is set and spend is comfortably under it.
    Ok,
    /// Spend has reached `BUDGET_APPROACHING_FRACTION` of the cap: outcome
    /// routing should start preferring cheaper reliable providers.
    Approaching,
    /// Spend has reached or passed the cap: further spend should be blocked.
    Exceeded,
}

/// Classify a session's spend against its (optional) cap. Pure fn.
///
/// A NaN or non-positive cap (only reachable via a hand-edited/corrupt file —
/// [`write_session_budget_cap`] rejects such values before they can be
/// persisted) can't meaningfully be "approached", so it's treated as already
/// `Exceeded` once anything has been spent, and `Ok` at zero spend — the
/// failure mode that can't silently let unbounded spend through. A positive
/// infinite cap is left to the normal comparisons below, where it behaves as
/// "no real limit" (any finite spend is `< cap`) without a special case.
pub fn budget_state(budget: &SessionBudget) -> BudgetState {
    let Some(cap) = budget.cap_usd else {
        return BudgetState::Unlimited;
    };
    if cap.is_nan() || cap <= 0.0 {
        return if budget.spent_usd > 0.0 {
            BudgetState::Exceeded
        } else {
            BudgetState::Ok
        };
    }
    if budget.spent_usd >= cap {
        BudgetState::Exceeded
    } else if budget.spent_usd >= cap * BUDGET_APPROACHING_FRACTION {
        BudgetState::Approaching
    } else {
        BudgetState::Ok
    }
}

/// [`pick_agent_by_outcome`] plus an optional budget bias (issue 006 full
/// scope). `budget: None` — and any `budget` whose `cap_usd` is `None` — is
/// byte-identical to `pick_agent_by_outcome` (same `pick_best` call, same
/// exponent, same reason text): the invariant that "caps off == today" holds
/// by construction, not by a separate branch that has to be kept in sync.
/// Once `budget_state` reads `Approaching` or `Exceeded`, scoring weights
/// cost more heavily so a cheaper reliable provider can win over the plain
/// cost-per-success winner; eligibility, the min-runs/recency gates, and
/// deterministic tie-breaks are otherwise unchanged.
pub fn pick_agent_by_outcome_with_budget(
    stats: &[OutcomeStats],
    registry: &Registry,
    now_ms: i64,
    budget: Option<&SessionBudget>,
) -> Option<OutcomePick> {
    let biased = matches!(
        budget.map(budget_state).unwrap_or(BudgetState::Unlimited),
        BudgetState::Approaching | BudgetState::Exceeded
    );
    let exponent = if biased { BUDGET_COST_BIAS_EXPONENT } else { 1.0 };
    pick_best(stats, registry, now_ms, exponent, biased)
}

/// On-disk schema for `~/.cortex/session-budgets/<session_id>.json`. Only
/// written when a user sets a cap for that session; a missing file means "no
/// cap" (`cap_usd: None`), which is byte-identical to today's routing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionBudgetFile {
    cap_usd: Option<f64>,
}

fn session_budgets_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("session-budgets"))
}

/// Session ids look like `session-<uuid>`; reject path separators/`..` (same
/// contract as `commands::focus_chain::is_valid_session_id`) to keep the
/// write contained to the session-budgets dir.
fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn session_budget_path(session_id: &str) -> Option<std::path::PathBuf> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    session_budgets_dir().map(|d| d.join(format!("{session_id}.json")))
}

/// Read the spend cap set for one session. Missing/unreadable/malformed file,
/// or an invalid session id, all resolve to `None` — "no cap", the only safe
/// failure mode. Pure read, no locking (mirrors `outcome_routing_enabled`).
pub fn session_budget_cap(session_id: &str) -> Option<f64> {
    let path = session_budget_path(session_id)?;
    let raw = std::fs::read_to_string(&path).ok()?;
    parse_session_budget(&raw).cap_usd
}

/// Parse the session-budget file body. Malformed JSON resolves to "no cap"
/// set (`cap_usd: None`), the same fail-safe default as
/// [`parse_outcome_routing`]. Pure.
fn parse_session_budget(raw: &str) -> SessionBudgetFile {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Persist (or clear, with `cap_usd: None`) the spend cap for one session.
/// Rejects a non-positive/non-finite cap outright so a corrupt value can
/// never be written in the first place — [`budget_state`]'s handling of that
/// case only exists as a defensive fallback for a hand-edited file.
pub fn write_session_budget_cap(session_id: &str, cap_usd: Option<f64>) -> anyhow::Result<()> {
    if let Some(cap) = cap_usd {
        if !cap.is_finite() || cap <= 0.0 {
            anyhow::bail!("invalid budget cap ${cap}: must be a positive, finite USD amount");
        }
    }
    let path = session_budget_path(session_id)
        .ok_or_else(|| anyhow::anyhow!("invalid session id '{session_id}'"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&SessionBudgetFile { cap_usd })?;
    std::fs::write(&path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::{
        AgentAdapter, AgentDescriptor, AgentEvent, ChatRequest,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Minimal adapter stub: a fixed descriptor (id, caps, availability) and a
    /// no-op `run` — `pick_model_for` only ever reads `descriptor()`.
    struct StubAdapter {
        id: &'static str,
        caps: Vec<AgentCapability>,
        available: bool,
    }

    #[async_trait]
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.id.to_string(),
                label: self.id.to_string(),
                description: String::new(),
                capabilities: self.caps.clone(),
                available: self.available,
            }
        }
        async fn health_check(&self) -> bool {
            self.available
        }
        async fn run(
            &self,
            _req: ChatRequest,
            _tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn reg_with(adapters: Vec<StubAdapter>) -> Registry {
        let mut r = Registry::new();
        for a in adapters {
            r.register(Arc::new(a));
        }
        r
    }

    fn claude_cli() -> StubAdapter {
        StubAdapter {
            id: "claude-cli",
            caps: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::ShellExec,
                AgentCapability::LongContext,
            ],
            available: true,
        }
    }

    fn gateway() -> StubAdapter {
        StubAdapter {
            id: "gateway-remote",
            caps: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::ShellExec,
                AgentCapability::LongContext,
            ],
            available: true,
        }
    }

    fn ollama() -> StubAdapter {
        StubAdapter {
            id: "ollama",
            caps: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::LongContext,
            ],
            available: true,
        }
    }

    fn anthropic_direct() -> StubAdapter {
        // Chat-only direct adapter: NO ShellExec/CodeEdit, mirroring the real
        // descriptor.
        StubAdapter {
            id: "anthropic_direct",
            caps: vec![AgentCapability::Chat, AgentCapability::LongContext],
            available: true,
        }
    }

    #[test]
    fn easy_chat_picks_the_cheapest_local_model() {
        // The gateway (cloud) + Ollama (local, free) both serve chat. Easy → free local.
        let reg = reg_with(vec![gateway(), ollama()]);
        let pick = pick_model_for(
            Difficulty::Easy,
            &[AgentCapability::Chat],
            &reg,
            &["ollama:llama3.2:1b".to_string()],
        )
        .expect("a chat model should be pickable");
        assert!(pick.local, "free local model must win on cost for easy work");
        assert_eq!(pick.model, "ollama:llama3.2:1b");
        assert_eq!(pick.agent_id, "ollama");
        assert_eq!(pick.price_sum(), 0.0);
    }

    #[test]
    fn easy_chat_without_local_picks_cheapest_cloud() {
        // No local tags supplied → cheapest catalog model on an available source.
        let reg = reg_with(vec![gateway()]);
        let pick = pick_model_for(Difficulty::Easy, &[AgentCapability::Chat], &reg, &[])
            .expect("a cloud chat model should be pickable");
        assert!(!pick.local);
        assert_eq!(pick.agent_id, "gateway-remote");
        // Cheapest gateway catalog model is gemini-3.1-flash-lite (0.10/0.40).
        let (inp, outp) = lookup_price(&pick.model);
        assert_eq!((inp, outp), (0.10, 0.40), "got {}", pick.model);
    }

    #[test]
    fn hard_picks_the_strongest_capable_model() {
        // claude-cli serves Opus (15/75) — the strongest in the catalog.
        let reg = reg_with(vec![claude_cli(), gateway(), ollama()]);
        let pick = pick_model_for(
            Difficulty::Hard,
            &[AgentCapability::Chat],
            &reg,
            &["ollama:llama3.2:1b".to_string()],
        )
        .expect("a chat model should be pickable");
        assert!(!pick.local, "hard work must not fall to a free local model");
        assert_eq!(pick.agent_id, "claude-cli");
        assert_eq!(pick.model, "claude-opus-4-8");
        assert_eq!(
            (pick.input_price_per_million_usd, pick.output_price_per_million_usd),
            (15.00, 75.00)
        );
    }

    #[test]
    fn repo_edit_never_returns_a_chat_only_adapter() {
        // A ShellExec-requiring task with ONLY a chat-only direct adapter (and a
        // chat/edit-only Ollama) available must yield nothing — never the
        // chat-only adapter.
        let reg = reg_with(vec![
            anthropic_direct(),
            ollama(),
        ]);
        let pick = pick_model_for(
            Difficulty::Hard,
            &[AgentCapability::ShellExec],
            &reg,
            &["ollama:llama3.2:1b".to_string()],
        );
        assert!(
            pick.is_none(),
            "no ShellExec-capable model available → must be None, got {pick:?}"
        );
    }

    #[test]
    fn repo_edit_routes_to_a_shellexec_capable_model() {
        // With claude-cli (ShellExec) AND a chat-only direct adapter present, a
        // repo-edit task must route to claude-cli, never the direct adapter.
        let reg = reg_with(vec![anthropic_direct(), claude_cli()]);
        let pick = pick_model_for(
            Difficulty::Hard,
            &[AgentCapability::ShellExec, AgentCapability::CodeEdit],
            &reg,
            &[],
        )
        .expect("claude-cli can shell-exec");
        assert_eq!(pick.agent_id, "claude-cli");
        assert_ne!(pick.agent_id, "anthropic_direct");
    }

    #[test]
    fn unavailable_sources_are_skipped() {
        // Gateway registered but UNAVAILABLE → its catalog models are not candidates.
        let unavail_gateway = StubAdapter {
            id: "gateway-remote",
            caps: vec![AgentCapability::Chat, AgentCapability::ShellExec],
            available: false,
        };
        let reg = reg_with(vec![unavail_gateway]);
        let pick = pick_model_for(Difficulty::Easy, &[AgentCapability::Chat], &reg, &[]);
        assert!(pick.is_none(), "an unavailable source yields no candidates");
    }

    #[test]
    fn empty_registry_yields_none() {
        let reg = reg_with(vec![]);
        assert!(
            pick_model_for(Difficulty::Easy, &[AgentCapability::Chat], &reg, &[]).is_none()
        );
    }

    // ────────── Cost-per-success (outcome-aware) routing — issue 006 ─────────

    const NOW: i64 = 1_700_000_000_000;

    fn stats(key: &str, finished_runs: u64, success_rate: f64, est_usd: f64) -> OutcomeStats {
        OutcomeStats {
            key: key.to_string(),
            finished_runs,
            success_rate,
            est_usd,
            last_run_ms: NOW - 60_000, // fresh: one minute ago
        }
    }

    #[test]
    fn outcome_score_is_success_rate_per_dollar_per_run() {
        // 10 runs, 90% success, $1 total → $0.10/run → score 9.0.
        let s = stats("gateway-remote", 10, 0.9, 1.0);
        let score = outcome_score(&s, NOW).expect("enough fresh data to score");
        assert!((score - 9.0).abs() < 1e-9, "got {score}");
    }

    #[test]
    fn outcome_score_thin_data_returns_none() {
        // One run below the gate → None, no matter how good it looks.
        let s = stats("gateway-remote", OUTCOME_MIN_RUNS - 1, 1.0, 0.0);
        assert_eq!(outcome_score(&s, NOW), None);
        // Exactly at the gate → scoreable.
        let s = stats("gateway-remote", OUTCOME_MIN_RUNS, 1.0, 1.0);
        assert!(outcome_score(&s, NOW).is_some());
    }

    #[test]
    fn outcome_score_stale_data_returns_none() {
        let mut s = stats("gateway-remote", 20, 1.0, 1.0);
        s.last_run_ms = NOW - OUTCOME_RECENCY_WINDOW_MS - 1;
        assert_eq!(outcome_score(&s, NOW), None);
        // Exactly on the window edge is still fresh.
        s.last_run_ms = NOW - OUTCOME_RECENCY_WINDOW_MS;
        assert!(outcome_score(&s, NOW).is_some());
    }

    #[test]
    fn outcome_score_free_provider_uses_cost_floor_not_infinity() {
        // $0 spend (local Ollama) must not divide by zero — it scores at the
        // floor: huge (free reliable providers should win) but finite.
        let s = stats("ollama", 25, 1.0, 0.0);
        let score = outcome_score(&s, NOW).expect("scoreable");
        assert!(score.is_finite());
        assert!((score - 1.0 / 0.0001).abs() < 1e-6, "got {score}");
    }

    #[test]
    fn failing_expensive_provider_loses_to_cheap_reliable_one() {
        // The acceptance criterion: with sufficient history, a provider that
        // fails often is de-prioritized vs a cheaper reliable one.
        let reg = reg_with(vec![claude_cli(), gateway()]);
        let all = vec![
            stats("claude-cli", 20, 0.5, 10.0),     // $0.50/run, coin-flip → 1.0
            stats("gateway-remote", 20, 0.95, 2.0), // $0.10/run, reliable → 9.5
        ];
        let pick = pick_agent_by_outcome(&all, &reg, NOW).expect("both scoreable");
        assert_eq!(pick.agent_id, "gateway-remote");
        assert!((pick.score - 9.5).abs() < 1e-9, "got {}", pick.score);
        assert!(
            pick.reason.contains("outcome-route → gateway-remote"),
            "rationale must surface the pick: {}",
            pick.reason
        );
        assert!(pick.reason.contains("95% success over 20 runs"), "{}", pick.reason);
    }

    #[test]
    fn pick_ignores_providers_the_registry_cannot_reach() {
        // "lmstudio" has stellar history but is not registered; the gateway is
        // registered but its stats are thin. → thin data overall → None.
        let reg = reg_with(vec![gateway()]);
        let all = vec![
            stats("lmstudio", 50, 1.0, 0.0),
            stats("gateway-remote", 2, 1.0, 0.1),
        ];
        assert_eq!(pick_agent_by_outcome(&all, &reg, NOW), None);
    }

    #[test]
    fn pick_ignores_unavailable_and_chat_incapable_adapters() {
        let unavailable = StubAdapter {
            id: "gateway-remote",
            caps: vec![AgentCapability::Chat],
            available: false,
        };
        let no_chat = StubAdapter {
            id: "claude-cli",
            caps: vec![AgentCapability::ShellExec],
            available: true,
        };
        let reg = reg_with(vec![unavailable, no_chat]);
        let all = vec![
            stats("gateway-remote", 20, 1.0, 1.0),
            stats("claude-cli", 20, 1.0, 1.0),
        ];
        assert_eq!(pick_agent_by_outcome(&all, &reg, NOW), None);
    }

    #[test]
    fn pick_returns_none_on_thin_or_stale_data() {
        let reg = reg_with(vec![claude_cli(), gateway()]);
        let mut stale = stats("claude-cli", 20, 1.0, 1.0);
        stale.last_run_ms = NOW - OUTCOME_RECENCY_WINDOW_MS - 1;
        let thin = stats("gateway-remote", OUTCOME_MIN_RUNS - 1, 1.0, 1.0);
        assert_eq!(pick_agent_by_outcome(&[stale, thin], &reg, NOW), None);
        assert_eq!(pick_agent_by_outcome(&[], &reg, NOW), None);
    }

    #[test]
    fn pick_tie_breaks_are_deterministic() {
        // Same score → higher success rate wins; fully identical → smaller id.
        let reg = reg_with(vec![claude_cli(), gateway()]);
        let a = stats("claude-cli", 10, 0.9, 1.0); // 0.9 / 0.1 = 9.0
        let b = stats("gateway-remote", 20, 0.45, 1.0); // 0.45 / 0.05 = 9.0
        let pick = pick_agent_by_outcome(&[a.clone(), b.clone()], &reg, NOW).unwrap();
        assert_eq!(pick.agent_id, "claude-cli", "higher success rate breaks the tie");
        // Order of the input slice must not matter.
        let pick2 = pick_agent_by_outcome(&[b, a], &reg, NOW).unwrap();
        assert_eq!(pick2.agent_id, "claude-cli");
    }

    #[test]
    fn outcome_stats_map_reliability_rows() {
        use crate::observability::tracing_store::ReliabilityRow;
        let row = ReliabilityRow {
            key: "gateway-remote".into(),
            agent_id: Some("gateway-remote".into()),
            model: None,
            runs: 12,
            ok_runs: 9,
            error_runs: 1,
            running_runs: 2,
            success_rate: 0.9,
            p50_ms: Some(100),
            p95_ms: Some(200),
            avg_ms: Some(120),
            total_tokens: 1000,
            est_usd: 0.5,
            top_error_class: None,
            last_run_ms: 42,
            last_error_span: None,
            last_error_session: None,
        };
        let stats = outcome_stats_from_reliability(&[row]);
        assert_eq!(
            stats,
            vec![OutcomeStats {
                key: "gateway-remote".into(),
                finished_runs: 10, // ok + error; running runs excluded
                success_rate: 0.9,
                est_usd: 0.5,
                last_run_ms: 42,
            }]
        );
    }

    #[test]
    fn outcome_routing_toggle_parses_and_defaults_off() {
        assert!(parse_outcome_routing(r#"{"enabled": true}"#).enabled);
        assert!(!parse_outcome_routing(r#"{"enabled": false}"#).enabled);
        // Malformed / empty bodies default OFF — the only safe failure mode
        // for an opt-in routing preference is "behave exactly like today".
        assert!(!parse_outcome_routing("").enabled);
        assert!(!parse_outcome_routing("{not json").enabled);
        assert!(!parse_outcome_routing(r#"{"enabled": "yes"}"#).enabled);
    }

    // ────────── Budget ceilings per session — issue 006 full scope ──────────

    fn budget(cap_usd: Option<f64>, spent_usd: f64) -> SessionBudget {
        SessionBudget { cap_usd, spent_usd }
    }

    #[test]
    fn budget_state_classifies_unlimited_ok_approaching_exceeded() {
        assert_eq!(budget_state(&budget(None, 0.0)), BudgetState::Unlimited);
        assert_eq!(budget_state(&budget(None, 999.0)), BudgetState::Unlimited);

        assert_eq!(budget_state(&budget(Some(10.0), 0.0)), BudgetState::Ok);
        assert_eq!(budget_state(&budget(Some(10.0), 7.99)), BudgetState::Ok);
        // Exactly at the approaching fraction (80%) tips into Approaching.
        assert_eq!(budget_state(&budget(Some(10.0), 8.0)), BudgetState::Approaching);
        assert_eq!(budget_state(&budget(Some(10.0), 9.5)), BudgetState::Approaching);
        // Exactly at the cap, and over it, are both Exceeded.
        assert_eq!(budget_state(&budget(Some(10.0), 10.0)), BudgetState::Exceeded);
        assert_eq!(budget_state(&budget(Some(10.0), 15.0)), BudgetState::Exceeded);
    }

    #[test]
    fn budget_state_treats_a_corrupt_cap_as_the_safe_extremes() {
        // Only reachable via a hand-edited file — write_session_budget_cap
        // rejects these before they can be persisted. Any spend at all against
        // a nonsensical cap must not look "fine"; zero spend is not yet a
        // problem.
        assert_eq!(budget_state(&budget(Some(0.0), 0.0)), BudgetState::Ok);
        assert_eq!(budget_state(&budget(Some(0.0), 0.01)), BudgetState::Exceeded);
        assert_eq!(budget_state(&budget(Some(-5.0), 1.0)), BudgetState::Exceeded);
        assert_eq!(budget_state(&budget(Some(f64::NAN), 1.0)), BudgetState::Exceeded);
        assert_eq!(budget_state(&budget(Some(f64::INFINITY), 1.0)), BudgetState::Ok);
    }

    #[test]
    fn pick_with_budget_none_is_byte_identical_to_plain_pick() {
        // The invariant: "caps off == today". No budget at all, and a budget
        // that merely has no cap set, must both match `pick_agent_by_outcome`
        // exactly (score AND rationale) over a variety of stats.
        let reg = reg_with(vec![claude_cli(), gateway()]);
        let cases: Vec<Vec<OutcomeStats>> = vec![
            vec![
                stats("claude-cli", 20, 0.5, 10.0),
                stats("gateway-remote", 20, 0.95, 2.0),
            ],
            vec![stats("claude-cli", 10, 0.99, 3.0), stats("gateway-remote", 10, 0.6, 2.0)],
            vec![],
        ];
        for case in cases {
            let plain = pick_agent_by_outcome(&case, &reg, NOW);
            let no_budget = pick_agent_by_outcome_with_budget(&case, &reg, NOW, None);
            let empty_cap = pick_agent_by_outcome_with_budget(
                &case,
                &reg,
                NOW,
                Some(&budget(None, 123.0)),
            );
            assert_eq!(no_budget, plain, "None budget must match plain pick");
            assert_eq!(empty_cap, plain, "cap_usd:None must match plain pick");
        }
    }

    #[test]
    fn approaching_budget_prefers_the_cheaper_reliable_provider() {
        // claude-cli: 99% success at $0.30/run → wins the plain (unbiased)
        // pick (score 3.3) over gateway-remote's 60% success at $0.20/run
        // (score 3.0).
        let reg = reg_with(vec![claude_cli(), gateway()]);
        let all = vec![
            stats("claude-cli", 10, 0.99, 3.0),
            stats("gateway-remote", 10, 0.60, 2.0),
        ];
        let plain = pick_agent_by_outcome(&all, &reg, NOW).expect("both scoreable");
        assert_eq!(plain.agent_id, "claude-cli", "expensive-but-reliable wins with no budget pressure");

        // Once spend is Approaching the session cap, squaring the cost term
        // flips the ranking: claude-cli's biased score (0.99/0.3^2 = 11.0) now
        // loses to gateway-remote's (0.60/0.2^2 = 15.0) — the cheaper provider,
        // still comfortably above a coin flip on success rate.
        let approaching = budget(Some(10.0), 9.0); // 90% spent ⇒ Approaching
        assert_eq!(budget_state(&approaching), BudgetState::Approaching);
        let biased = pick_agent_by_outcome_with_budget(&all, &reg, NOW, Some(&approaching))
            .expect("both still scoreable under the bias");
        assert_eq!(biased.agent_id, "gateway-remote");
        assert!(
            biased.reason.contains("budget: preferring cheaper reliable provider"),
            "{}",
            biased.reason
        );

        // Exceeded applies the same bias (the send itself is blocked by the
        // chat_send call site, not by pick_best).
        let exceeded = budget(Some(10.0), 10.0);
        assert_eq!(budget_state(&exceeded), BudgetState::Exceeded);
        let biased_exceeded =
            pick_agent_by_outcome_with_budget(&all, &reg, NOW, Some(&exceeded)).unwrap();
        assert_eq!(biased_exceeded.agent_id, "gateway-remote");
    }

    #[test]
    fn budget_bias_still_respects_the_eligibility_and_recency_gates() {
        // An unavailable adapter with a great budget-biased score still can't
        // win; thin/stale data is still gated out under a budget exactly as
        // without one.
        let unavailable = StubAdapter {
            id: "claude-cli",
            caps: vec![AgentCapability::Chat],
            available: false,
        };
        let reg = reg_with(vec![unavailable, gateway()]);
        let all = vec![
            stats("claude-cli", 50, 1.0, 0.01), // cheapest + most reliable, but down
            stats("gateway-remote", OUTCOME_MIN_RUNS - 1, 1.0, 0.01), // thin
        ];
        let exceeded = budget(Some(1.0), 1.0);
        assert_eq!(
            pick_agent_by_outcome_with_budget(&all, &reg, NOW, Some(&exceeded)),
            None
        );
    }

    #[test]
    fn session_budget_toggle_parses_and_defaults_to_no_cap() {
        assert_eq!(parse_session_budget(r#"{"cap_usd": 5.0}"#).cap_usd, Some(5.0));
        assert_eq!(parse_session_budget(r#"{"cap_usd": null}"#).cap_usd, None);
        assert_eq!(parse_session_budget("{}").cap_usd, None);
        // Malformed / empty bodies default to "no cap" — the only safe
        // failure mode for an opt-in spend guard is "behave like today".
        assert_eq!(parse_session_budget("").cap_usd, None);
        assert_eq!(parse_session_budget("{not json").cap_usd, None);
        assert_eq!(parse_session_budget(r#"{"cap_usd": "five"}"#).cap_usd, None);
    }

    #[test]
    fn session_budget_cap_rejects_invalid_values_before_persisting() {
        assert!(write_session_budget_cap("session-test-a", Some(0.0)).is_err());
        assert!(write_session_budget_cap("session-test-a", Some(-1.0)).is_err());
        assert!(write_session_budget_cap("session-test-a", Some(f64::NAN)).is_err());
        assert!(write_session_budget_cap("session-test-a", Some(f64::INFINITY)).is_err());
        // Path-unsafe session ids are rejected outright, never touching disk.
        assert!(write_session_budget_cap("../escape", Some(5.0)).is_err());
        assert!(write_session_budget_cap("has space", Some(5.0)).is_err());
        assert_eq!(session_budget_cap("../escape"), None);
    }

    #[test]
    fn session_budget_cap_round_trips_through_the_real_file() {
        // Uses the real `~/.cortex/session-budgets/` dir (same idiom as
        // `outcome_routing_enabled`'s tests would, had it needed round-trip
        // coverage) — a throwaway, clearly-scoped session id keeps this from
        // colliding with anything a real session would use.
        let id = "session-cost-router-budget-roundtrip-test";
        assert_eq!(session_budget_cap(id), None, "no cap set yet");
        write_session_budget_cap(id, Some(2.50)).expect("valid cap persists");
        assert_eq!(session_budget_cap(id), Some(2.50));
        write_session_budget_cap(id, None).expect("clearing the cap persists");
        assert_eq!(session_budget_cap(id), None);
        // Clean up so repeated test runs don't leave stray files behind.
        if let Some(path) = session_budget_path(id) {
            let _ = std::fs::remove_file(path);
        }
    }
}
