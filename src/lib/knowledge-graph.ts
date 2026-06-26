import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend types for the `build_knowledge_graph` Tauri command. Mirrors the
 * `Node` / `Edge` / `KnowledgeGraph` structs in
 * `src-tauri/src/commands/knowledge_graph.rs` — keep the two in sync.
 */
export interface GraphNode {
  id: string;
  label: string;
  path: string;
  source: string;
  /** File length in characters; drives the rendered radius. */
  size: number;
}

export interface GraphEdge {
  from: string;
  to: string;
}

export interface KnowledgeGraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** True when we hit the 500-node / 2000-edge backend cap. */
  truncated: boolean;
}

export async function buildKnowledgeGraph(
  activeProject?: string,
  obsidianVault?: string,
): Promise<KnowledgeGraph> {
  return invoke<KnowledgeGraph>("build_knowledge_graph", {
    activeProject: activeProject ?? null,
    obsidianVault: obsidianVault ?? null,
  });
}
