// Skills — Anthropic-style declarative prompt templates loaded from
// `~/.cortex/skills/<name>/SKILL.md`. The Rust loader parses the YAML
// frontmatter and exposes them via three commands; this module is a thin
// typed wrapper plus best-effort error handling (mirrors `lib/snippets.ts`).
//
// Surface:
//   - `listSkills`  → populate the panel on mount
//   - `getSkill`    → fetch a single skill (used for preview / refresh)
//   - `expandSkill` → substitute user-supplied vars into the template body
//
// All calls degrade to safe defaults (`[]` / `null`) on backend failure so a
// missing `~/.cortex/skills` dir doesn't take down the UI.

import { invoke } from "@tauri-apps/api/core";

/** A single declared input variable. `options` is empty for freeform text
 *  inputs; non-empty for enumerated `<select>` style choices. */
export interface SkillInput {
  name: string;
  options: string[];
}

/** Parsed SKILL.md record. The `body` is the unexpanded template — callers
 *  pass it through `expandSkill` to substitute `{{var}}` markers. */
export interface Skill {
  name: string;
  description: string;
  inputs: SkillInput[];
  body: string;
}

export async function listSkills(): Promise<Skill[]> {
  try {
    return await invoke<Skill[]>("list_skills");
  } catch {
    return [];
  }
}

export async function getSkill(name: string): Promise<Skill | null> {
  try {
    return await invoke<Skill | null>("get_skill", { name });
  } catch {
    return null;
  }
}

/**
 * Render a skill's body with the supplied variables. Returns `null` if the
 * backend rejected the call (missing var, unknown skill, …) — callers should
 * surface the failure in the UI rather than silently dropping it.
 */
export async function expandSkill(
  name: string,
  vars: Record<string, string>,
): Promise<string | null> {
  try {
    return await invoke<string>("expand_skill", { name, vars });
  } catch (err) {
    console.warn("expandSkill failed", err);
    return null;
  }
}
