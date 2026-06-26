//! Shared model pricing — the single source of truth for `$ / 1M tokens`
//! estimates used both by the cost *tracker* (after the fact: turns recorded
//! token totals into dollars) and the cost *router* (before the fact: pick the
//! cheapest / strongest model that satisfies a task's capability needs).
//!
//! Promoted out of `commands/cost_tracker.rs` (where it began life) so the
//! orchestrator's `cost_router` can price candidate models without depending on
//! the Tauri command layer. `cost_tracker` now re-imports from here.
//!
//! Prices are hard-coded 2026 estimates. When a real per-token usage schema
//! lands upstream, this table is the only place that needs updating.

/// `$ per 1M tokens` (input, output). Keys are matched case-insensitively as a
/// *prefix* of the agent_id / model name. The first prefix hit wins, so list
/// more specific entries before broader ones. Falls back to [`DEFAULT_PRICE`]
/// when nothing matches.
///
/// Kept roughly in step with `orchestrator::aliases::CATALOG` so the cost
/// router can tier the curated models meaningfully (flagship > mini, pro >
/// flash). The generic family prefixes (`claude-opus`, `gemini`, …) backstop
/// any dated/uncatalogued id that still shares a family.
pub const PRICING: &[(&str, (f64, f64))] = &[
    // ── Anthropic (current catalog ids, then family fallbacks) ──────────────
    ("claude-opus-4-8", (15.00, 75.00)),
    ("claude-opus-4-7", (15.00, 75.00)),
    ("claude-opus-4-6", (15.00, 75.00)),
    ("claude-sonnet-4-7", (3.00, 15.00)),
    ("claude-sonnet-4-6", (3.00, 15.00)),
    ("claude-haiku-4-5", (0.80, 4.00)),
    ("claude-haiku", (0.80, 4.00)),
    ("claude-sonnet", (3.00, 15.00)),
    ("claude-opus", (15.00, 75.00)),
    ("claude", (3.00, 15.00)),
    // ── OpenAI / Codex (specific → generic; mini before its base family) ────
    ("gpt-5.4-mini", (0.40, 1.60)),
    ("gpt-5.5", (10.00, 30.00)),
    ("gpt-5.4", (5.00, 15.00)),
    ("gpt-5", (5.00, 15.00)),
    ("gpt-4o-mini", (0.15, 0.60)),
    ("gpt-4o", (5.00, 20.00)),
    ("gpt-4.1", (3.00, 12.00)),
    ("gpt-4", (10.00, 30.00)),
    ("o1-mini", (3.00, 12.00)),
    ("o1", (15.00, 60.00)),
    ("codex", (3.00, 12.00)),
    // ── Google Gemini (lite/flash before pro; 3.x before family fallback) ───
    ("gemini-3.1-flash-lite", (0.10, 0.40)),
    ("gemini-3-flash", (0.30, 1.00)),
    ("gemini-3.1-pro", (3.50, 10.50)),
    ("gemini-3-pro", (3.50, 10.50)),
    ("gemini-2.5-flash", (0.35, 1.05)),
    ("gemini-2.5-pro", (3.50, 10.50)),
    ("gemini-flash", (0.35, 1.05)),
    ("gemini-pro", (3.50, 10.50)),
    ("gemini", (1.00, 4.00)),
];

/// Price used when nothing in [`PRICING`] matches the model id.
pub const DEFAULT_PRICE: (f64, f64) = (1.0, 4.0);

/// Case-insensitive *prefix* match against the pricing table. Any row whose key
/// is a prefix of the model id wins; the table is ordered specific→generic so
/// the first hit is the most precise. Unknown ids fall back to [`DEFAULT_PRICE`].
pub fn lookup_price(model: &str) -> (f64, f64) {
    let needle = model.to_ascii_lowercase();
    for (key, price) in PRICING {
        if needle.starts_with(&key.to_ascii_lowercase()) {
            return *price;
        }
    }
    DEFAULT_PRICE
}

/// Dollar cost of a (prompt, completion) token split at the given
/// `(input, output)` per-million price.
pub fn compute_usd(prompt_tokens: u64, completion_tokens: u64, price: (f64, f64)) -> f64 {
    let input = (prompt_tokens as f64) * price.0 / 1_000_000.0;
    let output = (completion_tokens as f64) * price.1 / 1_000_000.0;
    input + output
}

/// The tracing store records a single `total` count per run. Split into
/// input/output as 50/50 — close enough for cost estimates and consistent with
/// how most chat workloads land empirically (slightly output-heavy on
/// reasoning, input-heavy on bulk-paste; averages out).
pub fn split_tokens(total: u64) -> (u64, u64) {
    let prompt = total / 2;
    let completion = total - prompt;
    (prompt, completion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_price_finds_opus() {
        assert_eq!(lookup_price("claude-opus-4-8"), (15.00, 75.00));
        assert_eq!(lookup_price("claude-opus-4-7"), (15.00, 75.00));
    }

    #[test]
    fn lookup_price_finds_sonnet() {
        assert_eq!(lookup_price("claude-sonnet-4-6"), (3.00, 15.00));
    }

    #[test]
    fn lookup_price_finds_gemini_flash() {
        assert_eq!(lookup_price("gemini-2.5-flash"), (0.35, 1.05));
    }

    #[test]
    fn lookup_price_prices_current_catalog_ids() {
        // The router tiers these — flagship must out-price its mini sibling and
        // the flash/lite variants, or "strongest"/"cheapest" pick wrong.
        let flagship = lookup_price("gpt-5.5");
        let mini = lookup_price("gpt-5.4-mini");
        assert!(flagship.0 + flagship.1 > mini.0 + mini.1);
        let pro = lookup_price("gemini-3.1-pro-preview");
        let lite = lookup_price("gemini-3.1-flash-lite-preview");
        assert!(pro.0 + pro.1 > lite.0 + lite.1);
    }

    #[test]
    fn lookup_price_falls_back_to_default() {
        assert_eq!(lookup_price("totally-unknown-model"), DEFAULT_PRICE);
    }

    #[test]
    fn compute_usd_matches_hand_calc() {
        // 1M input @ $3 + 1M output @ $15 = $18
        let usd = compute_usd(1_000_000, 1_000_000, (3.0, 15.0));
        assert!((usd - 18.0).abs() < 1e-6);
    }

    #[test]
    fn split_tokens_handles_odd_totals() {
        let (p, c) = split_tokens(101);
        assert_eq!(p + c, 101);
        assert_eq!(p, 50);
        assert_eq!(c, 51);
    }
}
