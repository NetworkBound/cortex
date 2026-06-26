//! Unified model catalog + canonical alias resolution (OpenAI Codex CLI parity).
//!
//! Codex CLI keeps a built-in catalog that maps a user-typed model string to a
//! *provider* and a *canonical* upstream model name, and accepts short aliases
//! (`o3`, `gpt-4o`, …) that resolve to the full id. Cortex previously had only
//! brittle, lossy **prefix** matching scattered across `route()` and the static
//! tuples in `commands/models.rs` (e.g. `model.starts_with("opus")` — which
//! would also wrongly swallow a hypothetical `opus-mini`), and no way to type a
//! shorthand like `opus` or `gpt5` and have it resolve to the real id the
//! adapter/gateway expects.
//!
//! This module is the **single source of truth** for the curated catalog and for
//! resolving an arbitrary input string to a canonical model id. It is a pure,
//! total, network-free set of functions so the mapping is deterministically
//! unit-testable, and `commands/models.rs` builds its picker list from the same
//! catalog (no more drift between "what the picker offers" and "what resolves").

/// One curated model: its canonical id (what the adapter/gateway receives), a
/// friendly label for the picker, the adapter `source` that serves it
/// (`"claude-cli"` | `"gateway"`), and the short aliases that resolve to it.
#[derive(Debug, Clone, Copy)]
pub struct CatalogModel {
    pub id: &'static str,
    pub label: &'static str,
    /// Adapter that serves this model — matches the registry id used by
    /// `route()` (`"claude-cli"` for the local CLI, `"gateway"` for the gateway).
    pub source: &'static str,
    /// Lowercase short aliases that canonicalize to `id`. Must not collide with
    /// any other alias or any canonical id (guarded by a test). **Critically, an
    /// alias must never be the concrete id of a *distinct* model** — current or
    /// plausibly-future (e.g. `gpt-4`, `gpt-5`, `gpt-5-mini` are real OpenAI id
    /// shapes for models that are NOT this one, so they are not aliases): such an
    /// alias would silently re-route a user who typed one model to a different
    /// one, and would rewrite a live `/v1/models` id that collides with it. Only
    /// unambiguous *shorthands* belong here (`gpt`, `gpt5`, `4o`, `opus`).
    /// Guarded by `known_distinct_model_ids_never_canonicalize`.
    pub aliases: &'static [&'static str],
    /// The local AI-maker CLI adapter that *prefers* to serve this model when
    /// it's installed (`"codex-cli"`, `"gemini-cli"`, …), or `None` for models
    /// with no local CLI (Claude already uses `source: "claude-cli"` directly).
    ///
    /// `source` stays the reliable fallback (`"gateway"`); routing + cost_router
    /// prefer `cli_source` *only when that adapter is registered AND available*,
    /// so an uninstalled CLI cleanly falls back to the gateway. This is how a
    /// `gpt-*` slug "routes default-to-local" without ever stranding a user who
    /// hasn't installed the CLI.
    pub cli_source: Option<&'static str>,
}

/// The curated catalog. Ollama models are intentionally absent — they're
/// discovered live via `/api/tags` and carry an explicit `ollama:` prefix, so
/// they never need aliasing.
pub const CATALOG: &[CatalogModel] = &[
    // ── Anthropic / local Claude Code CLI ──────────────────────────────────
    CatalogModel {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
        source: "claude-cli",
        aliases: &["opus", "opus-4.8", "claude-opus"],
        cli_source: None,
    },
    CatalogModel {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        source: "claude-cli",
        // bare `claude` resolves to the balanced default (Sonnet).
        aliases: &["sonnet", "sonnet-4.6", "claude-sonnet", "claude"],
        cli_source: None,
    },
    CatalogModel {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        source: "claude-cli",
        aliases: &["haiku", "haiku-4.5", "claude-haiku"],
        cli_source: None,
    },
    // ── Gemini (Cortex Gateway credential pool) ────────────────────────────
    CatalogModel {
        id: "gemini-3.1-pro-preview",
        label: "Gemini 3.1 Pro",
        source: "gateway",
        // bare `gemini` resolves to the flagship Pro.
        aliases: &["gemini", "gemini-pro", "gemini-3.1-pro"],
        cli_source: Some("gemini-cli"),
    },
    CatalogModel {
        id: "gemini-3-pro-preview",
        label: "Gemini 3 Pro",
        source: "gateway",
        aliases: &["gemini-3-pro"],
        cli_source: Some("gemini-cli"),
    },
    CatalogModel {
        id: "gemini-3-flash-preview",
        label: "Gemini 3 Flash",
        source: "gateway",
        aliases: &["gemini-flash", "flash", "gemini-3-flash"],
        cli_source: Some("gemini-cli"),
    },
    CatalogModel {
        id: "gemini-3.1-flash-lite-preview",
        label: "Gemini 3.1 Flash Lite",
        source: "gateway",
        aliases: &["gemini-flash-lite", "flash-lite"],
        cli_source: Some("gemini-cli"),
    },
    // ── OpenAI / Codex (Cortex Gateway, ChatGPT account) ───────────────────
    CatalogModel {
        id: "gpt-5.5",
        label: "GPT-5.5",
        source: "gateway",
        // bare `gpt` resolves to the flagship 5.5. Note: `gpt-5` is intentionally
        // NOT an alias — it's a plausible distinct-model id shape, not a shorthand.
        aliases: &["gpt", "gpt5", "gpt5.5"],
        cli_source: Some("codex-cli"),
    },
    CatalogModel {
        id: "gpt-5.4",
        label: "GPT-5.4",
        source: "gateway",
        aliases: &["gpt5.4"],
        cli_source: Some("codex-cli"),
    },
    CatalogModel {
        id: "gpt-5.4-mini",
        label: "GPT-5.4 Mini",
        source: "gateway",
        // `gpt-5-mini` is intentionally NOT an alias — it's a plausible distinct
        // OpenAI model id, not a shorthand for the 5.4 mini.
        aliases: &["gpt5.4-mini", "gpt5-mini"],
        cli_source: Some("codex-cli"),
    },
    CatalogModel {
        id: "gpt-4.1",
        label: "GPT-4.1",
        source: "gateway",
        aliases: &["gpt4.1", "gpt-41"],
        cli_source: Some("codex-cli"),
    },
    CatalogModel {
        id: "gpt-4o",
        label: "GPT-4o",
        source: "gateway",
        // `gpt-4` is intentionally NOT an alias — it's a real, distinct OpenAI
        // model; aliasing it here silently re-routed `gpt-4` requests to `gpt-4o`.
        aliases: &["gpt4o", "4o"],
        cli_source: Some("codex-cli"),
    },
    CatalogModel {
        id: "gpt-4o-mini",
        label: "GPT-4o Mini",
        source: "gateway",
        aliases: &["gpt4o-mini", "4o-mini"],
        cli_source: Some("codex-cli"),
    },
];

/// Resolve an arbitrary input string to a canonical catalog id.
///
/// The lookup is case-insensitive and whitespace-trimmed. An input that already
/// *is* a canonical id resolves to itself (idempotent); a known short alias
/// resolves to its canonical id. Anything unrecognized — an Ollama slug, a
/// live-discovered gateway model, a typo — returns `None`, so callers leave it
/// untouched rather than fabricating a mapping.
pub fn canonicalize(raw: &str) -> Option<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    for m in CATALOG {
        if m.id == key {
            return Some(m.id.to_string());
        }
        if m.aliases.iter().any(|a| *a == key) {
            return Some(m.id.to_string());
        }
    }
    None
}

/// Resolve `raw` to its canonical id, or pass it through unchanged (trimmed)
/// when it isn't a known model/alias. This is the function the chat + arena
/// paths use to normalize the model that reaches the adapter/gateway: a typed
/// `opus` becomes `claude-opus-4-8`, while an `ollama:llama3` or any unknown
/// slug flows through verbatim.
pub fn resolve_model(raw: &str) -> String {
    canonicalize(raw).unwrap_or_else(|| raw.trim().to_string())
}

/// The adapter `source` that serves a model, resolving aliases first. Returns
/// `None` for anything not in the catalog. `route()` uses this to pick the
/// claude-cli adapter deterministically from the catalog rather than by string
/// prefix.
pub fn source_of(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    CATALOG
        .iter()
        .find(|m| m.id == key || m.aliases.iter().any(|a| *a == key))
        .map(|m| m.source)
}

/// The local AI-maker CLI adapter that *prefers* to serve `raw` (resolving
/// aliases first), or `None` when the model has no associated local CLI. This
/// is the "route default-to-local" hook: `route()` / `cost_router` prefer this
/// CLI when its adapter is registered AND available, else fall back to
/// [`source_of`] (the gateway). Returns `None` for unknown inputs.
pub fn cli_source_of(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    CATALOG
        .iter()
        .find(|m| m.id == key || m.aliases.iter().any(|a| *a == key))
        .and_then(|m| m.cli_source)
}

/// `(id, label)` pairs for every catalog model served by `source`, in catalog
/// order. `commands/models.rs` builds its picker groups from these so the picker
/// and the resolver can never disagree.
pub fn models_for_source(source: &str) -> Vec<(&'static str, &'static str)> {
    CATALOG
        .iter()
        .filter(|m| m.source == source)
        .map(|m| (m.id, m.label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn canonical_ids_are_idempotent() {
        for m in CATALOG {
            assert_eq!(canonicalize(m.id).as_deref(), Some(m.id), "id {} not idempotent", m.id);
        }
    }

    #[test]
    fn every_alias_resolves_to_its_id() {
        for m in CATALOG {
            for a in m.aliases {
                assert_eq!(
                    canonicalize(a).as_deref(),
                    Some(m.id),
                    "alias {a} should resolve to {}",
                    m.id
                );
            }
        }
    }

    #[test]
    fn resolution_is_case_and_whitespace_insensitive() {
        assert_eq!(canonicalize("  Opus ").as_deref(), Some("claude-opus-4-8"));
        assert_eq!(canonicalize("GPT5").as_deref(), Some("gpt-5.5"));
        assert_eq!(canonicalize("Claude-Sonnet-4-6").as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn common_shorthands_map_to_flagships() {
        assert_eq!(canonicalize("claude").as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(canonicalize("gpt").as_deref(), Some("gpt-5.5"));
        assert_eq!(canonicalize("gemini").as_deref(), Some("gemini-3.1-pro-preview"));
        assert_eq!(canonicalize("flash").as_deref(), Some("gemini-3-flash-preview"));
    }

    #[test]
    fn unknown_inputs_do_not_canonicalize() {
        assert_eq!(canonicalize(""), None);
        assert_eq!(canonicalize("   "), None);
        // A near-miss prefix must NOT match (the old prefix-routing bug).
        assert_eq!(canonicalize("opus-mini"), None);
        assert_eq!(canonicalize("ollama:llama3.2"), None);
        assert_eq!(canonicalize("some-future-model"), None);
    }

    #[test]
    fn resolve_model_passes_unknowns_through_trimmed() {
        // alias → canonical
        assert_eq!(resolve_model("opus"), "claude-opus-4-8");
        // already canonical → unchanged
        assert_eq!(resolve_model("gpt-5.5"), "gpt-5.5");
        // unknown → trimmed passthrough, never rewritten
        assert_eq!(resolve_model("  ollama:qwen2.5  "), "ollama:qwen2.5");
        assert_eq!(resolve_model("totally-made-up"), "totally-made-up");
    }

    #[test]
    fn source_lookup_resolves_aliases() {
        assert_eq!(source_of("opus"), Some("claude-cli"));
        assert_eq!(source_of("claude-haiku-4-5"), Some("claude-cli"));
        assert_eq!(source_of("gpt5"), Some("gateway"));
        assert_eq!(source_of("gemini"), Some("gateway"));
        assert_eq!(source_of("ollama:llama3"), None);
        assert_eq!(source_of("unknown"), None);
    }

    #[test]
    fn no_alias_collides_with_an_id_or_another_alias() {
        // Every alias and every id must be globally unique — a collision would
        // make resolution order-dependent and silently mis-route a model.
        let mut seen: HashSet<&str> = HashSet::new();
        for m in CATALOG {
            assert!(seen.insert(m.id), "duplicate canonical id: {}", m.id);
        }
        for m in CATALOG {
            for a in m.aliases {
                assert!(
                    seen.insert(a),
                    "alias {a} collides with an id or another alias",
                );
                // aliases must be lowercase so the case-insensitive lookup is exact
                assert_eq!(*a, a.to_ascii_lowercase(), "alias {a} must be lowercase");
            }
        }
    }

    #[test]
    fn conflated_distinct_model_aliases_are_gone() {
        // Regression for the MEDIUM review finding: these id-shaped strings used
        // to be aliases that silently re-routed a distinct model to another
        // (`gpt-4`→gpt-4o, `gpt-5`→gpt-5.5, `gpt-5-mini`→gpt-5.4-mini). They must
        // no longer canonicalize — left untouched, they pass through to whatever
        // adapter/gateway actually serves them (or simply 404 if nothing does),
        // never a different model.
        assert_eq!(canonicalize("gpt-4"), None, "gpt-4 must not re-route to gpt-4o");
        assert_eq!(canonicalize("gpt-5"), None, "gpt-5 must not re-route to gpt-5.5");
        assert_eq!(canonicalize("gpt-5-mini"), None, "gpt-5-mini must not re-route");
        // resolve_model now leaves them verbatim (the picker/resolver drift fix:
        // a live `/v1/models` id matching one of these is no longer rewritten).
        assert_eq!(resolve_model("gpt-4"), "gpt-4");
        assert_eq!(resolve_model("gpt-5"), "gpt-5");
        assert_eq!(resolve_model("gpt-5-mini"), "gpt-5-mini");
        // The unambiguous shorthands that share a flagship still resolve.
        assert_eq!(canonicalize("gpt").as_deref(), Some("gpt-5.5"));
        assert_eq!(canonicalize("gpt5").as_deref(), Some("gpt-5.5"));
        assert_eq!(canonicalize("4o").as_deref(), Some("gpt-4o"));
        assert_eq!(canonicalize("gpt4o").as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn known_distinct_model_ids_never_canonicalize() {
        // A curated list of real, concrete provider model ids that are NOT in our
        // catalog. None may canonicalize to a catalog id: doing so would silently
        // substitute a model the user did not ask for (and rewrite a live picker
        // id on send). This locks the "an alias is never a distinct model's id"
        // invariant so a future alias addition can't reintroduce the conflation.
        const KNOWN_DISTINCT_IDS: &[&str] = &[
            "gpt-4",
            "gpt-4-turbo",
            "gpt-4-32k",
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-3.5-turbo",
            "o1",
            "o1-mini",
            "o3",
            "o3-mini",
            "o4-mini",
            "chatgpt-4o-latest",
            "claude-3-5-sonnet-20241022",
            "claude-3-opus-20240229",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
        ];
        for id in KNOWN_DISTINCT_IDS {
            assert_eq!(
                canonicalize(id),
                None,
                "{id} is a distinct model — it must not canonicalize to a catalog id"
            );
            // and resolve_model must leave it byte-for-byte (trimmed) untouched.
            assert_eq!(&resolve_model(id), id, "{id} must pass through verbatim");
        }
    }

    #[test]
    fn catalog_entries_are_populated() {
        for m in CATALOG {
            assert!(!m.id.is_empty(), "empty id");
            assert!(!m.label.is_empty(), "empty label for {}", m.id);
            assert!(
                m.source == "claude-cli" || m.source == "gateway",
                "unexpected source {} for {}",
                m.source,
                m.id
            );
        }
    }

    #[test]
    fn models_for_source_splits_the_catalog() {
        let claude = models_for_source("claude-cli");
        assert_eq!(claude.len(), 3);
        assert!(claude.iter().any(|(id, _)| *id == "claude-opus-4-8"));
        let gateway = models_for_source("gateway");
        assert!(gateway.iter().any(|(id, _)| *id == "gpt-5.5"));
        assert!(gateway.iter().all(|(id, _)| !id.starts_with("claude")));
    }
}
