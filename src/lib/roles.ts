import { invoke } from "@tauri-apps/api/core";

/**
 * Mirror of `agents::roles::Role`. A "role" is a re-usable agent persona
 * stored at `~/.cortex/roles/<name>.yaml`. Optional fields are omitted from
 * the wire format when `None` on the Rust side.
 */
export interface Role {
  name: string;
  description?: string;
  tools?: string[];
  model?: string;
  system_prompt?: string;
}

/** List every role under `~/.cortex/roles/*.yaml`. Returns `[]` when empty. */
export async function listRoles(): Promise<Role[]> {
  return invoke<Role[]>("list_roles");
}

/** Create or update a role on disk. Returns the persisted role. */
export async function setRole(role: Role): Promise<Role> {
  return invoke<Role>("set_role", { role });
}

/** Delete a role file. Missing files are a no-op. */
export async function deleteRole(name: string): Promise<void> {
  return invoke("delete_role", { name });
}

/**
 * Apply a role's `system_prompt` to the agent identified by `agentId`. Routes
 * through the existing per-agent custom-instructions storage so the chat
 * pipeline picks it up automatically. Returns the prompt as persisted (empty
 * string if the role had no system prompt).
 */
export async function applyRoleToAgent(
  roleName: string,
  agentId: string,
): Promise<string> {
  return invoke<string>("apply_role_to_agent", { roleName, agentId });
}

// ── Profile bundling (Codex #10) ────────────────────────────────────────────
