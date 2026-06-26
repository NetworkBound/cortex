/**
 * TS bridge for the recipe-gallery Tauri commands. Mirrors the Rust
 * `Recipe` struct shape verbatim so the gallery modal can render without
 * any post-processing.
 *
 * Recipes are stored at `~/.cortex/recipes/<name>.yaml`. The backend
 * enforces the validation (name regex, 64 KiB cap, https-only installs);
 * this module is just a thin invoke wrapper plus a few seed URLs for the
 * "Browse community" tab.
 */

import { invoke } from "@tauri-apps/api/core";

export interface Recipe {
  name: string;
  description: string;
  goal: string;
  tools: string[];
  agents: string[];
  /** Free-form per-checkpoint hooks. Not interpreted client-side. */
  checkpoints: unknown;
  path: string;
  yaml: string;
}

export async function listRecipes(): Promise<Recipe[]> {
  return invoke<Recipe[]>("list_recipes");
}

export async function getRecipe(name: string): Promise<Recipe | null> {
  return invoke<Recipe | null>("get_recipe", { name });
}

export async function saveRecipe(name: string, yaml: string): Promise<Recipe> {
  return invoke<Recipe>("save_recipe", { name, yaml });
}

export async function deleteRecipe(name: string): Promise<void> {
  return invoke<void>("delete_recipe", { name });
}

export async function installRecipeFromUrl(url: string): Promise<Recipe> {
  return invoke<Recipe>("install_recipe_from_url", { url });
}

/** Stub "community" recipe URLs surfaced in the gallery modal. We don't
 *  host a real gallery service yet, so these are illustrative pointers —
 *  the install flow itself fetches whatever HTTPS YAML you point at, so
 *  users can paste their own URLs into the Install field. */
export interface CommunityRecipeSeed {
  label: string;
  description: string;
  url: string;
}
export const COMMUNITY_SEEDS: CommunityRecipeSeed[] = [
  {
    label: "deploy-staging.yaml",
    description: "Reference deploy recipe (SSH + pull + restart).",
    url: "https://raw.githubusercontent.com/cortex-app/recipes/main/deploy-staging.yaml",
  },
  {
    label: "weekly-changelog.yaml",
    description: "Auto-generate a weekly changelog from git history.",
    url: "https://raw.githubusercontent.com/cortex-app/recipes/main/weekly-changelog.yaml",
  },
  {
    label: "test-and-ship.yaml",
    description: "Run tests, commit, push, tag — fail-fast on red.",
    url: "https://raw.githubusercontent.com/cortex-app/recipes/main/test-and-ship.yaml",
  },
];

/** Template seeded into the "New recipe" editor when there's no existing
 *  body to edit. Mirrors the format expected by `parse_yaml` server-side
 *  so a fresh recipe round-trips without surprises. */
export const NEW_RECIPE_TEMPLATE = `name: my-recipe
description: One-line summary of what this recipe does.
goal: "Describe what the agent should achieve."
tools: []
agents: []
checkpoints:
  - on_complete: notify
`;
