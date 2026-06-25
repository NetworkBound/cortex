import { invoke } from "@tauri-apps/api/core";

/**
 * Mirror of `orchestrator::profiles::Profile`. Optional fields are omitted
 * from the wire format when None on the Rust side.
 */
export interface Profile {
  name: string;
  model?: string;
  reasoning_effort?: "low" | "medium" | "high";
  sandbox_tier?: "read-only" | "workspace-write" | "danger-full-access";
  allowed_tools?: string[];
  system_prompt?: string;
}

/**
 * List every profile under `<project_root>/.cortex/profiles/*.toml`.
 * Returns `[]` when the directory is missing — that's not an error.
 */
export async function listProfiles(projectRoot: string): Promise<Profile[]> {
  return invoke<Profile[]>("list_profiles", { projectRoot });
}

/**
 * Apply a profile by filename stem. Backend mutates `AppState::config`
 * with the profile's model / sandbox / reasoning settings and returns the
 * loaded profile so the UI can show what just got applied.
 */
export async function applyProfile(
  projectRoot: string,
  name: string,
): Promise<Profile> {
  return invoke<Profile>("apply_profile", { projectRoot, name });
}

/**
 * Fetch the saved per-agent custom instructions for `agentId`. Empty values
 * on disk are normalized to `null` so the UI can treat "unset" and "empty
 * textarea" identically.
 *
 * Storage: `~/.cortex/agent-instructions.json` (a flat `{ [agent_id]: string }`).
 */
export async function getAgentInstructions(
  agentId: string,
): Promise<string | null> {
  return invoke<string | null>("get_agent_instructions", { agentId });
}

/**
 * Persist `text` as the custom instructions for `agentId`. An empty / blank
 * `text` clears the entry. Returns the trimmed value that landed on disk
 * (empty string after a clear), so the editor can show "saved: <preview>".
 */
export async function setAgentInstructions(
  agentId: string,
  text: string,
): Promise<string> {
  return invoke<string>("set_agent_instructions", { agentId, text });
}
