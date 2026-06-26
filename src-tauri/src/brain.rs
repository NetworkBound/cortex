//! "Brain" aggregator — recent sessions, projects, and memory hits in one
//! response. Surfaced as the BrainPanel in the UI.

use crate::memory::{markdown::read_entry, sources};
use crate::observability::tracing_store::TracingStore;
use crate::projects::discover_projects;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct BrainSnapshot {
    pub recent_projects: Vec<RecentProject>,
    pub recent_sessions: Vec<RecentSession>,
    pub recent_memory: Vec<RecentMemory>,
    pub obsidian_vault: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentProject {
    pub root: PathBuf,
    pub name: String,
    pub last_modified_ms: i64,
    pub has_git: bool,
    pub has_claude_md: bool,
    pub has_runbooks: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentSession {
    pub session_id: String,
    pub last_active_ms: i64,
    pub message_count: i64,
    pub agents: Vec<String>,
    pub first_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentMemory {
    pub path: String,
    pub title: Option<String>,
    pub source: String,
    pub modified_unix_ms: i64,
    pub preview: String,
}

pub fn build_snapshot(
    store: &TracingStore,
    obsidian_vault: Option<&std::path::Path>,
) -> BrainSnapshot {
    let recent_projects = discover_projects(obsidian_vault.map(|p| p.to_path_buf()))
        .into_iter()
        .take(20)
        .map(|p| RecentProject {
            root: p.root,
            name: p.name,
            last_modified_ms: p.last_modified_ms,
            has_git: p.has_git,
            has_claude_md: p.has_claude_md,
            has_runbooks: p.has_runbooks,
        })
        .collect();

    let recent_sessions = store
        .recent_sessions(30)
        .ok()
        .unwrap_or_default()
        .into_iter()
        .map(|s| RecentSession {
            session_id: s.session_id,
            last_active_ms: s.last_active_ms,
            message_count: s.message_count,
            agents: s.agents,
            first_message: s.first_message,
        })
        .collect();

    let sources = sources::default_sources(None, obsidian_vault);
    let mut memory_hits: Vec<RecentMemory> = Vec::new();
    for src in sources.iter().take(8) {
        for p in sources::walk_markdown(src).into_iter().take(20) {
            if let Ok(entry) = read_entry(&p) {
                let preview: String = entry
                    .body
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
                memory_hits.push(RecentMemory {
                    path: entry.path.display().to_string(),
                    title: entry.title,
                    source: src.label.clone(),
                    modified_unix_ms: entry.modified_unix_ms,
                    preview,
                });
            }
        }
    }
    memory_hits.sort_by(|a, b| b.modified_unix_ms.cmp(&a.modified_unix_ms));
    memory_hits.truncate(40);

    BrainSnapshot {
        recent_projects,
        recent_sessions,
        recent_memory: memory_hits,
        obsidian_vault: obsidian_vault.map(|p| p.display().to_string()),
    }
}
