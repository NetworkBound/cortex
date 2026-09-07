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

/**
 * Returns the merged-with-scope-headers text Cortex injects into the
 * system prompt at session bootstrap. Empty string when no AGENTS.md
 * files exist anywhere — callers should fall back to their own context.
 */
export async function agentsMdMerged(
  projectRoot: string,
  cwd?: string,
): Promise<string> {
  return invoke<string>("agents_md_merged", { projectRoot, cwd: cwd ?? null });
}

/** Human-readable label for a scope. Order matches precedence. */
export const SCOPE_LABEL: Record<AgentsDocSegment["scope"], string> = {
  global: "~/.cortex/AGENTS.md",
  codex: "~/.codex/AGENTS.md",
  project: "project AGENTS.md",
  cortex: ".cortex/AGENTS.md",
  cwd: "cwd AGENTS.md",
};
