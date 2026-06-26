use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry of all known agents. Built once at app start.
pub struct Registry {
    agents: HashMap<String, Arc<dyn AgentAdapter>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { agents: HashMap::new() }
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
