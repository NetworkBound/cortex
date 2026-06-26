import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend bridge for the Spaces backend. A "space" is a scoped subset of a
 * project (e.g. frontend/backend/docs) defined by glob includes + excludes
 * stored at `<project_root>/.cortex/spaces.yaml`. The panel reads/writes via
 * these helpers; nothing else in the app talks to the spaces command set.
 *
 * The shape mirrors the Rust `Space` struct exactly so we can pass values
 * straight through `invoke` without intermediate mapping.
 */

export interface Space {
  name: string;
  description: string;
  includes: string[];
  excludes: string[];
}

/** Returns `[]` when `<project>/.cortex/spaces.yaml` is missing. */
export async function listSpaces(projectRoot: string): Promise<Space[]> {
  return invoke<Space[]>("list_spaces", { projectRoot });
}

/** Upsert by name (case-insensitive). Creates `.cortex/` if missing. */
export async function saveSpace(projectRoot: string, space: Space): Promise<void> {
  return invoke<void>("save_space", { projectRoot, space });
}

/** Idempotent delete — no-op if the name isn't present. */
export async function deleteSpace(projectRoot: string, name: string): Promise<void> {
  return invoke<void>("delete_space", { projectRoot, name });
}

/**
 * Resolve the glob filter against the project's file walk and return matching
 * relative paths (sorted). Use a high `limit` for the browse modal.
 */
export async function spaceFiles(
  projectRoot: string,
  name: string,
  limit?: number,
): Promise<string[]> {
  return invoke<string[]>("space_files", { projectRoot, name, limit: limit ?? null });
}

/** Build a fresh Space with sensible empty defaults — handy for new-space forms. */
export function emptySpace(): Space {
  return { name: "", description: "", includes: [], excludes: [] };
}

/**
 * Coerce a textarea with one glob per line into a clean string array.
 * Empty lines and pure whitespace are dropped so users can type freely.
 */
export function parseGlobLines(raw: string): string[] {
  return raw
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Inverse of parseGlobLines — for editing an existing space. */
export function formatGlobLines(globs: string[]): string {
  return globs.join("\n");
}
