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

/** Load a single role by filename stem. Throws on missing / malformed. */
export async function getRole(name: string): Promise<Role> {
  return invoke<Role>("get_role", { name });
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

/**
 * Per-dimension report from `apply_profile_v2`. Each entry in `applied` names
 * a dimension that was successfully bundled (model / sandbox_mode /
 * approval_policy / reasoning_effort). `errors` collects per-dimension
 * failure messages so partial success is visible to the UI.
 */
export interface ProfileApplyResult {
  applied: string[];
  errors: string[];
  name: string;
}

/**
 * Bundle-apply a profile: model + sandbox mode + approval policy + reasoning
 * effort in a single call. The classic `apply_profile` only mutates AppState;
 * the v2 variant additionally persists sandbox + approval policy to disk so
 * they survive a relaunch.
 */
export async function applyProfileV2(
  projectRoot: string,
  name: string,
): Promise<ProfileApplyResult> {
  return invoke<ProfileApplyResult>("apply_profile_v2", { projectRoot, name });
}
