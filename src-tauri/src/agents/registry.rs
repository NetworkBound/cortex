use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry of all known agents. Built once at app start.
pub struct Registry {
    agents: HashMap<String, Arc<dyn AgentAdapter>>,
    /// Last-observed reachability per adapter id, written by
    /// `commands::agents::check_agent_health` every time the UI's periodic
    /// polling actually probes an adapter. Consulted by
    /// `orchestrator::adapter_available` as an OVERRIDE on top of
    /// `AgentDescriptor.available` — not a replacement for it.
    ///
    /// WHY THIS EXISTS: `AgentDescriptor.available` is a synchronous trait
    /// method (`descriptor()`, not `async`), so adapters that can only judge
    /// reachability via a network probe (local-runtime endpoints — LM
    /// Studio/vLLM/TabbyAPI/Text-Gen-WebUI, see `agents::local_runtime`'s
    /// doc comment) have no choice but to report a cheap, optimistic
    /// `available: true` there and do the real check in `health_check()`
    /// instead. Historically NOTHING called `health_check()` before routing
    /// picked an adapter (only this cache's writer, the UI health-poll
    /// command, ever invoked it) — so a dead local runtime could still be
    /// explicitly picked/`@`-mentioned/default-routed to, only failing with
    /// a clear error at actual dispatch time. This cache closes that gap
    /// WITHOUT making routing itself async: it's a passive, best-effort
    /// record of what the periodic health poll already observed, checked
    /// synchronously. No cache entry (never polled) → behavior is
    /// UNCHANGED (falls back to `.available`, exactly as before this).
    health_cache: RwLock<HashMap<String, bool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { agents: HashMap::new(), health_cache: RwLock::new(HashMap::new()) }
    }

    /// Record the most recent live `health_check()` result for `id`. Called
    /// by `commands::agents::check_agent_health` after every real probe.
    pub fn record_health(&self, id: &str, healthy: bool) {
        self.health_cache.write().insert(id.to_string(), healthy);
    }

    /// The most recently observed reachability for `id`, or `None` if it has
    /// never been health-checked. Callers should treat `None` as "unknown,
    /// trust the descriptor's own `available`" — this is an override, not a
    /// primary signal.
    pub fn known_reachable(&self, id: &str) -> Option<bool> {
        self.health_cache.read().get(id).copied()
    }

    pub fn register(&mut self, agent: Arc<dyn AgentAdapter>) {
        let id = agent.descriptor().id.clone();
        self.agents.insert(id, agent);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.agents.get(id).cloned()
    }

    pub fn list_descriptors(&self) -> Vec<AgentDescriptor> {
        self.agents.values().map(|a| a.descriptor()).collect()
    }

    pub fn get_capabilities(&self, id_or_label: &str) -> Option<Vec<AgentCapability>> {
        // Resolve deterministically: prefer an exact id match (ids are the
        // registry keys and thus unique) before falling back to a label match.
        // This avoids nondeterministic resolution when an id/label collides
        // across agents, since `self.agents` iterates in arbitrary order.
        if let Some(a) = self.agents.get(id_or_label) {
            return Some(a.descriptor().capabilities);
        }
        for a in self.agents.values() {
            let d = a.descriptor();
            if d.label == id_or_label {
                return Some(d.capabilities);
            }
        }
        None
    }
}

impl Default for Registry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_reachable_is_none_until_recorded() {
        let reg = Registry::new();
        assert_eq!(reg.known_reachable("lmstudio"), None);
    }

    #[test]
    fn record_health_round_trips_and_overwrites() {
        let reg = Registry::new();
        reg.record_health("lmstudio", true);
        assert_eq!(reg.known_reachable("lmstudio"), Some(true));
        reg.record_health("lmstudio", false);
        assert_eq!(reg.known_reachable("lmstudio"), Some(false));
        // Independent per id.
        assert_eq!(reg.known_reachable("vllm"), None);
    }
}
