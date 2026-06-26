//! Cortex orchestrator — picks which agent handles a user message.
//!
//! Since Cortex now delegates everything to the Cortex Gateway (which is
//! itself an orchestrator over Claude/Codex/Gemini/Ollama via its
//! credential_pool), routing here is much simpler than before:
//!
//!   1. Explicit agent in `preferred_agent` (UI picker) if available.
//!   2. Default: `gateway-remote`.
//!
//! Fan-out across multiple Cortex-side adapters is no longer needed —
//! The gateway does that internally. We keep the trait + registry surface so
//! adding a future second adapter (e.g., a direct Ollama route) is one
//! file + one registry line.

pub mod aliases;
pub mod architect;
pub mod approval_policy;
pub mod approvals;
pub mod auto_approve;
pub mod cost_router;
pub mod guardrails;
pub mod profiles;
pub mod reasoning;
pub mod safe_commands;
pub mod sandbox;
pub mod team_run;
pub mod teams;
pub mod trust;
pub mod ultimate;

pub use approval_policy::{load_policy, write_policy, ApprovalPolicy, PolicyOutcome};
pub use approvals::{ApprovalRules, Decision};
pub use auto_approve::{AutoApproveEntry, AutoApproveList};
pub use guardrails::{Guardrails, Risk};
pub use profiles::{
    compose_system_prompt, get_agent_instructions, list_profiles, load_profile,
    set_agent_instructions, Profile,
};
pub use safe_commands::{extract_command, is_read_only_command};
pub use sandbox::{load_tier, tier_allows, write_tier, SandboxTier};
pub use trust::{is_trusted, trust_path, untrust_path};

use crate::agents::{AgentAdapter, ChatRequest, ChatTurn, Registry};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub agents: Vec<String>,
    pub stripped_message: String,
    pub reason: String,
}

/// Is an adapter with id `id` registered AND reporting itself available?
fn adapter_available(registry: &Registry, id: &str) -> bool {
    registry
        .list_descriptors()
        .into_iter()
        .any(|d| d.id == id && d.available)
}

pub fn route(req: &ChatRequest, registry: &Registry, preferred_agent: Option<String>) -> RoutingDecision {
    let msg = req.message.trim().to_string();

    if let Some(id) = preferred_agent {
        if registry.get(&id).is_some() {
            return RoutingDecision {
                agents: vec![id.clone()],
                stripped_message: msg,
                reason: format!("explicit pick: {id}"),
            };
        }
    }

    // Exact adapter-id match: a `model` that literally names a registered,
    // available adapter routes straight to it. No production composer model is
    // ever exactly an adapter id (`claude-opus-4-8` ≠ `claude-cli`), so this is
    // inert for real picks; it lets internal callers (the team manager planning
    // through `agents::oneshot`) target a specific adapter — notably the
    // CORTEX_E2E-only `e2e-fake` — by passing its id as the model. Sits after
    // the explicit picker (an explicit pick still wins) and before the slug
    // heuristics.
    if let Some(model) = req.model.as_deref() {
        let m = model.trim();
        if !m.is_empty()
            && registry
                .list_descriptors()
                .into_iter()
                .any(|d| d.id == m && d.available)
        {
            return RoutingDecision {
                agents: vec![m.to_string()],
                stripped_message: msg,
                reason: format!("model-route → exact adapter id {m}"),
            };
        }
    }

    // Model-based route → prefer the LOCAL CLI for any catalog model whose CLI
    // is installed. This sits AFTER the explicit picker (an explicit pick still
    // wins) but BEFORE the gateway default. Generalizes the old Claude-only
    // special case: a `gpt-*`/`gemini-*`/… slug now spawns its maker's local CLI
    // when available, and cleanly falls back to the gateway/direct path when not.
    if let Some(model) = req.model.as_deref() {
        let m = model.trim().to_lowercase();

        // (a) Claude: catalog `source == "claude-cli"`, plus a prefix fallback
        // for dated slugs not in the curated catalog (`claude-3-5-sonnet-…`).
        let claude_like = aliases::source_of(&m) == Some("claude-cli")
            || m.starts_with("claude")
            || m.starts_with("opus")
            || m.starts_with("sonnet")
            || m.starts_with("haiku");
        if claude_like && adapter_available(registry, "claude-cli") {
            return RoutingDecision {
                agents: vec!["claude-cli".to_string()],
                stripped_message: msg,
                reason: "model-route → claude-cli".to_string(),
            };
        }

        // (b) Any other maker (OpenAI/Codex, Gemini, Qwen, Grok, Mistral): the
        // catalog names the model's preferred local `cli_source`. Route there
        // when that adapter is installed; otherwise fall through to the gateway.
        if let Some(cli) = aliases::cli_source_of(&m) {
            if adapter_available(registry, cli) {
                return RoutingDecision {
                    agents: vec![cli.to_string()],
                    stripped_message: msg,
                    reason: format!("model-route → {cli}"),
                };
            }
        }

        // Ollama model slugs (`ollama:` / `ollama/`) route to the local Ollama
        // adapter when it's available.
        if (m.starts_with("ollama:") || m.starts_with("ollama/"))
            && adapter_available(registry, "ollama")
        {
            return RoutingDecision {
                agents: vec!["ollama".to_string()],
                stripped_message: msg,
                reason: "model-route → ollama".to_string(),
            };
        }
    }

    let default = "gateway-remote".to_string();
    let target = registry
        .list_descriptors()
        .into_iter()
        .find(|d| d.id == default && d.available)
        .map(|d| d.id)
        .or_else(|| registry.list_descriptors().into_iter().find(|d| d.available).map(|d| d.id))
        .unwrap_or(default);

    RoutingDecision {
        agents: vec![target.clone()],
        stripped_message: msg,
        reason: format!("default → {target}"),
    }
}

/// Distinct case-insensitive complexity keywords. Matched as substrings of the
/// lowercased message; multi-word phrases ("root cause", "race condition") are
/// matched whole.
const COMPLEX_KEYWORDS: &[&str] = &[
    "refactor",
    "architect",
    "design",
    "implement",
    "debug",
    "optimize",
    "migrate",
    "diagnose",
    "root cause",
    "performance",
    "concurrency",
    "race condition",
    "security",
    "vulnerability",
    "algorithm",
    "analyze",
    "review",
];

/// True when `msg` carries code-ish signals: a fenced block, common
/// language keywords/operators, a `/path/like.rs` token, or a stack-trace word.
fn message_has_code(msg: &str) -> bool {
    if msg.contains("```") {
        return true;
    }
    let lower = msg.to_lowercase();
    const SIGNALS: &[&str] = &[
        "fn ", "def ", "class ", "import ", "=>", "();", "panic", "traceback", "exception",
    ];
    if SIGNALS.iter().any(|s| lower.contains(s)) {
        return true;
    }
    // A `/path/like.rs` token: any whitespace-delimited word with a leading '/'
    // (or containing one) plus a file extension dot.
    msg.split_whitespace().any(|w| {
        let looks_pathy = w.contains('/') && w.contains('.');
        looks_pathy && w.len() > 3
    })
}

/// Deterministic, network-free model picker used when the user is in Auto mode
/// (no explicit model). Returns `(model_id, reason)`.
///
/// Only fires when an available `claude-cli` adapter is registered — that's the
/// reliable local path. Otherwise returns `None` so the caller keeps the
/// existing default (the Cortex Gateway).
pub fn auto_select_model(
    message: &str,
    history: &[ChatTurn],
    registry: &Registry,
) -> Option<(String, String)> {
    let has_claude_cli = registry
        .list_descriptors()
        .into_iter()
        .any(|d| d.id == "claude-cli" && d.available);
    if !has_claude_cli {
        return None;
    }

    let mut words = message.split_whitespace().count();
    let mut has_code = message_has_code(message);
    let lower = message.to_lowercase();
    let mut complex_kw = COMPLEX_KEYWORDS.iter().filter(|k| lower.contains(**k)).count();

    // If the message itself is very short, fold in the last user turn of
    // history so a terse follow-up to a meaty question isn't under-served.
    if words < 4 {
        if let Some(last_user) = history.iter().rev().find(|t| t.role == "user") {
            let l = last_user.content.to_lowercase();
            words = words.max(last_user.content.split_whitespace().count());
            has_code = has_code || message_has_code(&last_user.content);
            complex_kw = complex_kw
                .max(COMPLEX_KEYWORDS.iter().filter(|k| l.contains(**k)).count());
        }
    }

    // HIGH → opus, LOW → haiku, else MED → sonnet.
    if has_code || words > 80 || complex_kw >= 2 {
        Some((
            "claude-opus-4-8".to_string(),
            "auto: high complexity → claude-opus-4-8".to_string(),
        ))
    } else if words <= 8 && complex_kw == 0 && !has_code {
        Some((
            "claude-haiku-4-5".to_string(),
            "auto: low complexity → claude-haiku-4-5".to_string(),
        ))
    } else {
        Some((
            "claude-sonnet-4-6".to_string(),
            "auto: medium complexity → claude-sonnet-4-6".to_string(),
        ))
    }
}

pub fn resolve<'a>(
    decision: &'a RoutingDecision,
    registry: &'a Registry,
) -> Vec<Arc<dyn AgentAdapter>> {
    decision
        .agents
        .iter()
        .filter_map(|id| registry.get(id))
        .collect()
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::agents::{AgentCapability, AgentDescriptor, AgentEvent};
    use async_trait::async_trait;
    use tokio::sync::mpsc;

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
        async fn run(&self, _req: ChatRequest, _tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn reg_with(ids: &[(&str, bool)]) -> Registry {
        let mut reg = Registry::new();
        for (id, available) in ids {
            reg.register(Arc::new(StubAdapter {
                id: (*id).to_string(),
                available: *available,
            }));
        }
        reg
    }

    fn req_for(model: Option<&str>) -> ChatRequest {
        ChatRequest {
            session_id: String::new(),
            message: "hi".into(),
            project_root: None,
            history: Vec::new(),
            model: model.map(|m| m.to_string()),
            reasoning_effort: None,
        }
    }

    #[test]
    fn canonical_claude_id_routes_to_claude_cli() {
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("claude-opus-4-8")), &reg, None);
        assert_eq!(d.agents, vec!["claude-cli".to_string()]);
    }

    #[test]
    fn catalog_alias_routes_to_claude_cli() {
        // The composer canonicalizes before route(), but route() is also called
        // directly (arena) with raw input — so a catalog alias must route too.
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        for slug in ["sonnet", "haiku", "Opus", "claude"] {
            let d = route(&req_for(Some(slug)), &reg, None);
            assert_eq!(d.agents, vec!["claude-cli".to_string()], "{slug} → claude-cli");
        }
    }

    #[test]
    fn gateway_catalog_model_routes_to_gateway_default() {
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        for slug in ["gpt-5.5", "gpt5", "gemini", "gemini-3.1-pro-preview"] {
            let d = route(&req_for(Some(slug)), &reg, None);
            assert_eq!(d.agents, vec!["gateway-remote".to_string()], "{slug} → gateway");
        }
    }

    #[test]
    fn gpt_slug_routes_to_codex_cli_when_installed() {
        // A gpt-* slug prefers the local Codex CLI when its adapter is available.
        let reg = reg_with(&[("codex-cli", true), ("gateway-remote", true)]);
        for slug in ["gpt-5.5", "gpt", "gpt4o", "gpt-4o"] {
            let d = route(&req_for(Some(slug)), &reg, None);
            assert_eq!(d.agents, vec!["codex-cli".to_string()], "{slug} → codex-cli");
        }
    }

    #[test]
    fn gemini_slug_routes_to_gemini_cli_when_installed() {
        let reg = reg_with(&[("gemini-cli", true), ("gateway-remote", true)]);
        for slug in ["gemini", "gemini-3.1-pro-preview", "flash"] {
            let d = route(&req_for(Some(slug)), &reg, None);
            assert_eq!(d.agents, vec!["gemini-cli".to_string()], "{slug} → gemini-cli");
        }
    }

    #[test]
    fn gpt_slug_falls_back_to_gateway_when_codex_absent() {
        // codex-cli not registered → gpt-* cleanly falls back to the gateway.
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("gpt-5.5")), &reg, None);
        assert_eq!(d.agents, vec!["gateway-remote".to_string()]);
        // Registered but unavailable also falls back.
        let reg2 = reg_with(&[("codex-cli", false), ("gateway-remote", true)]);
        let d2 = route(&req_for(Some("gpt-5.5")), &reg2, None);
        assert_eq!(d2.agents, vec!["gateway-remote".to_string()]);
    }

    #[test]
    fn claude_route_falls_back_to_gateway_when_cli_absent() {
        // claude-cli registered but unavailable → don't route there.
        let reg = reg_with(&[("claude-cli", false), ("gateway-remote", true)]);
        let d = route(&req_for(Some("opus")), &reg, None);
        assert_eq!(d.agents, vec!["gateway-remote".to_string()]);
    }

    #[test]
    fn ollama_slug_routes_to_ollama() {
        let reg = reg_with(&[("ollama", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("ollama:llama3.2")), &reg, None);
        assert_eq!(d.agents, vec!["ollama".to_string()]);
    }

    #[test]
    fn explicit_pick_wins_over_model_route() {
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("opus")), &reg, Some("gateway-remote".to_string()));
        assert_eq!(d.agents, vec!["gateway-remote".to_string()]);
    }

    #[test]
    fn exact_adapter_id_model_routes_to_that_adapter() {
        // A model that literally names a registered, available adapter routes
        // straight to it — the hook the team manager uses to target `e2e-fake`.
        let reg = reg_with(&[("e2e-fake", true), ("claude-cli", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("e2e-fake")), &reg, None);
        assert_eq!(d.agents, vec!["e2e-fake".to_string()]);
        // An unavailable exact-id match is NOT used (falls through to the slug
        // heuristics / default).
        let reg2 = reg_with(&[("e2e-fake", false), ("gateway-remote", true)]);
        let d2 = route(&req_for(Some("e2e-fake")), &reg2, None);
        assert_eq!(d2.agents, vec!["gateway-remote".to_string()]);
    }

    #[test]
    fn dated_claude_slug_still_routes_via_prefix_fallback() {
        // Not in the curated catalog, but the prefix fallback keeps it on the CLI.
        let reg = reg_with(&[("claude-cli", true), ("gateway-remote", true)]);
        let d = route(&req_for(Some("claude-3-5-sonnet-20241022")), &reg, None);
        assert_eq!(d.agents, vec!["claude-cli".to_string()]);
    }
}
