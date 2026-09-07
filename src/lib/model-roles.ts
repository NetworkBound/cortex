import { invoke } from "@tauri-apps/api/core";

/**
 * Continue.dev-style per-project **model roles**: a default model id per logical
 * role, persisted at `<project_root>/.cortex/model-roles.toml`. Each role is
 * optional; an unset role falls through to the per-turn pick / Auto-selection
 * (chat) or the built-in default (planner/editor). See
 * `commands/model_roles.rs`.
 */
export interface ModelRoles {
  /** Default chat model (loses to an explicit composer pick, wins over Auto). */
  chat?: string | null;
  /** Default architect *planner* model (the strong-reasoning phase). */
  planner?: string | null;
  /** Default architect *editor* model (the execution phase). */
  editor?: string | null;
}

/** The role keys, in display order. */
export const MODEL_ROLE_KEYS = ["chat", "planner", "editor"] as const;
export type ModelRoleKey = (typeof MODEL_ROLE_KEYS)[number];

/** Human-facing label + one-line help per role, for the settings UI. */
export const MODEL_ROLE_META: Record<ModelRoleKey, { label: string; help: string }> = {
  chat: {
    label: "Chat",
    help: "Default model for normal chat turns. An explicit composer pick still wins; this beats Auto-selection.",
  },
  planner: {
    label: "Architect · Planner",
    help: "Model that drafts the plan in architect mode (the strong-reasoning phase).",
  },
  editor: {
    label: "Architect · Editor",
    help: "Model that executes the plan in architect mode (the editing phase).",
  },
};

/**
 * Load the configured model-role map for a project. A blank root or missing
 * config yields an empty map (best-effort: returns `{}` on failure).
 */
export async function getModelRoles(projectRoot: string | undefined | null): Promise<ModelRoles> {
  if (!projectRoot) return {};
  try {
    return await invoke<ModelRoles>("get_model_roles", { projectRoot });
  } catch {
    return {};
  }
}

/**
 * Persist the model-role map for a project. Blank fields clear that role; an
 * all-empty map clears the config file. Returns the map as stored.
 */
export async function setModelRoles(
  projectRoot: string,
  roles: ModelRoles,
): Promise<ModelRoles> {
  return invoke<ModelRoles>("set_model_roles", { projectRoot, roles });
}
