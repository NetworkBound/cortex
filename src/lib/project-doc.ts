import { invoke } from "@tauri-apps/api/core";

/**
 * One AGENTS.md file found by the hierarchical loader. The Rust side
 * always returns paths in this order: global → codex → project → cortex
 * → cwd. Bodies are pre-capped at 16 KiB on the backend.
 *
 * Scope labels mirror `commands/project_doc.rs::AgentsDocSegment::scope`.
 */
export interface AgentsDocSegment {
  path: string;
  body: string;
  scope: "global" | "codex" | "project" | "cortex" | "cwd";
}

/**
 * Returns every AGENTS.md file Cortex would inject for this project, in
 * precedence order. Missing files are silently skipped — an empty array
 * means the user has none configured at any layer.
 */
export async function agentsMdStack(
  projectRoot: string,
  cwd?: string,
): Promise<AgentsDocSegment[]> {
  return invoke<AgentsDocSegment[]>("agents_md_stack", {
    projectRoot,
    cwd: cwd ?? null,
  });
}
