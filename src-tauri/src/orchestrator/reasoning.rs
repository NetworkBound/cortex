//! Per-request reasoning-effort resolution (OpenAI Codex CLI parity).
//!
//! Codex CLI exposes a `model_reasoning_effort` knob — `minimal | low | medium
//! | high` — that tunes how much the upstream reasoning model "thinks" before
//! answering. Cortex already carried a *global* `reasoning_effort` on
//! `AppState::config` (set from a profile or the `CORTEX_REASONING_EFFORT` env
//! var) but never forwarded it to an individual chat request, and gave the user
//! no per-prompt control.
//!
//! This module is the single source of truth for *validating* and *resolving*
//! the effective effort for one request: a per-prompt override (from the
//! composer's picker) wins over the global config default; anything that isn't a
//! recognized level is dropped (treated as "unset"), so a malformed value can
//! never reach an upstream as an invalid parameter.
//!
//! It is deliberately a pure, total function with no I/O so the decision is
//! deterministically unit-testable.

/// The canonical Codex reasoning levels, in ascending order of effort. `minimal`
/// is Codex's lightest setting (added beyond the profile system's historical
/// `low|medium|high`); we accept it here for full parity.
const LEVELS: [&str; 4] = ["minimal", "low", "medium", "high"];

/// Normalize a raw effort string to one of the canonical levels.
///
/// Trims surrounding whitespace and lowercases, so `" High "` → `"high"`.
/// Returns `None` for empty input or any value outside [`LEVELS`] — callers
/// treat `None` as "no reasoning-effort hint" and omit the parameter entirely.
pub fn normalize(raw: &str) -> Option<String> {
    let cleaned = raw.trim().to_ascii_lowercase();
    if cleaned.is_empty() {
        return None;
    }
    LEVELS
        .iter()
        .find(|level| **level == cleaned)
        .map(|level| (*level).to_string())
}

/// Resolve the effective reasoning effort for a single request.
///
/// Precedence (highest first):
///   1. `per_request` — the composer's per-prompt picker.
///   2. `global` — the active profile / `CORTEX_REASONING_EFFORT` default.
///
/// Each candidate is normalized; an invalid candidate is skipped (it does *not*
/// shadow a valid lower-precedence one — a bogus per-request value falls through
/// to the global default rather than wiping it). Returns `None` when neither
/// yields a recognized level.
pub fn resolve(per_request: Option<&str>, global: Option<&str>) -> Option<String> {
    per_request
        .and_then(normalize)
        .or_else(|| global.and_then(normalize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_all_canonical_levels() {
        for level in ["minimal", "low", "medium", "high"] {
            assert_eq!(normalize(level).as_deref(), Some(level));
        }
    }

    #[test]
    fn normalize_trims_and_lowercases() {
        assert_eq!(normalize("  High ").as_deref(), Some("high"));
        assert_eq!(normalize("MEDIUM").as_deref(), Some("medium"));
    }

    #[test]
    fn normalize_rejects_unknown_and_empty() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("extreme"), None);
        assert_eq!(normalize("none"), None);
        // a numeric/garbage value never reaches an upstream as a param
        assert_eq!(normalize("3"), None);
    }

    #[test]
    fn resolve_per_request_wins_over_global() {
        assert_eq!(
            resolve(Some("high"), Some("low")).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn resolve_falls_back_to_global_when_no_override() {
        assert_eq!(resolve(None, Some("medium")).as_deref(), Some("medium"));
    }

    #[test]
    fn resolve_invalid_override_falls_through_to_global() {
        // a malformed per-request value must not shadow a valid global default
        assert_eq!(
            resolve(Some("turbo"), Some("low")).as_deref(),
            Some("low")
        );
        // empty-string override likewise falls through
        assert_eq!(resolve(Some(""), Some("high")).as_deref(), Some("high"));
    }

    #[test]
    fn resolve_none_when_nothing_valid() {
        assert_eq!(resolve(None, None), None);
        assert_eq!(resolve(Some("bogus"), Some("alsobogus")), None);
        assert_eq!(resolve(Some("bogus"), None), None);
    }
}
