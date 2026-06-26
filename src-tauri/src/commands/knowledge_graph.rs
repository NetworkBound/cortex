//! `build_knowledge_graph` — walks every memory source, parses
//! `[[wikilinks]]`, and returns a node/edge graph the frontend can render
//! as a force-directed visualization.
//!
//! Each markdown file becomes a `Node` (with its char count as `size`).
//! Each `[[wikilink]]` produces an `Edge` from the source file's stem to
//! the link target (also a stem). We cap at 500 nodes + 2000 edges so the
//! frontend's O(n^2) force simulation stays comfortable.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::app_state::AppState;
use crate::memory::{markdown, sources};
use serde::Serialize;
use tauri::State;

const MAX_NODES: usize = 500;
const MAX_EDGES: usize = 2000;

#[derive(Debug, Serialize)]
pub struct Node {
    /// Stem-ish id used by edges (file path's `file_stem`, lowercased).
    pub id: String,
    /// Display label (the file_stem as-is for readability).
    pub label: String,
    /// Absolute path on disk so the frontend can open the file in the editor.
    pub path: String,
    /// Source label this file came from (e.g. "runbooks", "obsidian:vault").
    pub source: String,
    /// File length in characters — drives node radius on the frontend.
    pub size: usize,
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Whether we truncated the result (hit the 500-node / 2000-edge caps).
    pub truncated: bool,
}

fn resolve_vault(arg: Option<String>, state: &State<'_, AppState>) -> Option<PathBuf> {
    if let Some(s) = arg {
        if !s.trim().is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    state.config.read().obsidian_vault.clone()
}

/// Lowercase, trimmed file-stem. Wikilink targets are normalised the same
/// way so `[[Project Cortex]]` matches `project_cortex.md` (Obsidian's
/// loose-matching convention).
fn normalise_id(raw: &str) -> String {
    raw.trim().to_lowercase()
}

fn stem_of(path: &std::path::Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[tauri::command]
pub async fn build_knowledge_graph(
    active_project: Option<String>,
    obsidian_vault: Option<String>,
    state: State<'_, AppState>,
) -> Result<KnowledgeGraph, String> {
    let active = active_project.as_ref().map(PathBuf::from);
    let vault = resolve_vault(obsidian_vault, &state);
    let srcs = sources::default_sources(active.as_deref(), vault.as_deref());

    // First pass — build nodes (one per unique file path). We dedupe by
    // canonical path because the same file can appear in multiple sources
    // (e.g. a project's CLAUDE.md and the global CLAUDE.md symlink).
    let mut nodes: Vec<Node> = Vec::new();
    let mut id_by_path: HashMap<PathBuf, String> = HashMap::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    // Wikilink edges are emitted only if both sides resolve to a known
    // node id. Build a stem→id lookup so different files with the same
    // stem (rare but possible) don't collide silently.
    let mut id_for_stem: HashMap<String, String> = HashMap::new();
    // Capture wikilinks alongside their source id so we can resolve them
    // after every node id is known.
    let mut pending: Vec<(String, Vec<String>)> = Vec::new();

    let mut truncated = false;

    'sources: for src in &srcs {
        for path in sources::walk_markdown(src) {
            if id_by_path.contains_key(&path) {
                continue;
            }
            let Some(stem) = stem_of(&path) else { continue };
            let mut id = normalise_id(&stem);
            // Disambiguate stem collisions by appending a short suffix.
            if seen_ids.contains(&id) {
                let suffix = id_by_path.len();
                id = format!("{id}#{suffix}");
            }

            let entry = match markdown::read_entry(&path) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let size = entry.body.chars().count();
            let label = entry.title.clone().unwrap_or(stem.clone());

            let node = Node {
                id: id.clone(),
                label,
                path: path.display().to_string(),
                source: src.label.clone(),
                size,
            };
            nodes.push(node);
            seen_ids.insert(id.clone());
            id_by_path.insert(path.clone(), id.clone());
            id_for_stem.entry(normalise_id(&stem)).or_insert(id.clone());
            pending.push((id, entry.wikilinks));

            if nodes.len() >= MAX_NODES {
                truncated = true;
                break 'sources;
            }
        }
    }

    // Second pass — resolve wikilinks. A `[[Foo]]` link tries:
    //   1. the exact normalised stem match,
    //   2. the link with spaces→underscores (Obsidian's filename munging),
    //   3. fall back to creating no edge (dangling links are common).
    // We also drop self-edges and dedupe to keep the SVG render cheap.
    let mut edges: Vec<Edge> = Vec::new();
    let mut edge_set: HashSet<(String, String)> = HashSet::new();

    'edges: for (from_id, wikilinks) in pending {
        for wl in wikilinks {
            // Obsidian supports `[[Target|Display]]` aliases — only the
            // part before the pipe is the link target.
            let target = wl.split('|').next().unwrap_or(&wl);
            // Strip the leading file path if present (`[[notes/foo]]`).
            let target = target.rsplit('/').next().unwrap_or(target);
            // Strip an optional .md suffix some authors include.
            let target = target.trim_end_matches(".md");

            let key1 = normalise_id(target);
            let key2 = key1.replace(' ', "_");
            let key3 = key1.replace(' ', "-");

            let to_id = id_for_stem
                .get(&key1)
                .or_else(|| id_for_stem.get(&key2))
                .or_else(|| id_for_stem.get(&key3))
                .cloned();

            let Some(to_id) = to_id else { continue };
            if to_id == from_id {
                continue;
            }
            let key = (from_id.clone(), to_id.clone());
            if !edge_set.insert(key) {
                continue;
            }
            edges.push(Edge {
                from: from_id.clone(),
                to: to_id,
            });
            if edges.len() >= MAX_EDGES {
                truncated = true;
                break 'edges;
            }
        }
    }

    Ok(KnowledgeGraph {
        nodes,
        edges,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_id_trims_and_lowercases() {
        assert_eq!(normalise_id("  Foo Bar  "), "foo bar");
        assert_eq!(normalise_id("CAPS"), "caps");
    }
}
