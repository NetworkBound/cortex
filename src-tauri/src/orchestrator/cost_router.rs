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
use crate::orchestrator::aliases::CATALOG;
use crate::pricing::lookup_price;

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
fn candidates(
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
    match difficulty {
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
    }
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
}
