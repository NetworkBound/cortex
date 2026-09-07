import { invoke } from "@tauri-apps/api/core";

/**
 * Fetch a human-readable repo map for the given project root.
 * Delegates to the Rust `repo_map_text` Tauri command.
 *
 * The returned string is intended to be injected as a system message so
 * the model has structural context (file tree, key symbols, etc.) about
 * the project the user is working in.
 */
export async function repoMapText(projectRoot: string): Promise<string> {
  return invoke<string>("repo_map_text", { projectRoot });
}
