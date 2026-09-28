import { invoke } from "@tauri-apps/api/core";

/**
 * Entry in the user-global auto-approve allowlist
 * (`~/.cortex/auto-approve.json`).
 *
 *  - `tool`:    case-insensitive match against the tool name; `""` is a wildcard
 *  - `pattern`: glob (`globset` crate semantics on the backend) matched
 *               against the tool call's primary string field (`command` /
 *               `cmd` / `shell` / `bash` / `path` / `file`) or the whole
 *               serialized payload when none of those fields are present
 *  - `profile`: optional tag — surfaced in the UI but not yet enforced
 */
export interface AutoApproveEntry {
  tool: string;
  pattern: string;
  profile?: string;
}

/** Read the on-disk allowlist. Missing file → `[]`. */
export async function listAutoApprove(): Promise<AutoApproveEntry[]> {
  return invoke<AutoApproveEntry[]>("list_auto_approve");
}

/** Append an entry. Backend validates `pattern` as a glob — bad globs reject. */
export async function addAutoApprove(entry: AutoApproveEntry): Promise<void> {
  return invoke("add_auto_approve", {
    tool: entry.tool,
    pattern: entry.pattern,
    profile: entry.profile ?? null,
  });
}

/** Remove the entry at `index` (0-based against `listAutoApprove()`). */
export async function removeAutoApprove(index: number): Promise<void> {
  return invoke("remove_auto_approve", { index });
}

/**
 * Best-effort guess of the natural glob pattern for a tool-call payload.
 * Mirrors the backend's `payload_for_match` so the "Always allow" button
 * suggests a pattern the user can immediately read and tweak.
 */
export function guessAutoApprovePattern(payload: unknown): string {
  if (payload && typeof payload === "object") {
    const obj = payload as Record<string, unknown>;
    for (const key of ["command", "cmd", "shell", "bash", "path", "file"]) {
      const v = obj[key];
      if (typeof v === "string" && v.trim()) {
        // First whitespace-separated token + `*` is the usual "let through
        // this family of calls" suggestion (e.g. `git status*`).
        const head = v.trim().split(/\s+/)[0];
        return head ? `${head}*` : v.trim();
      }
    }
  }
  if (typeof payload === "string" && payload.trim()) return payload.trim();
  return "*";
}
