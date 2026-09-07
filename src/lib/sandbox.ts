import { invoke } from "@tauri-apps/api/core";

/**
 * Codex-style three-tier sandbox. Mirrors `SandboxTier` in
 * `src-tauri/src/orchestrator/sandbox.rs`.
 *
 *  - `read-only`          — only read-shaped tools (read/search/ls/grep/…).
 *  - `workspace-write`    — read + write/edit/patch/run_*, writes must stay
 *                           inside the project root.
 *  - `danger-full-access` — anything goes; no gate.
 */
export type SandboxTier = "read-only" | "workspace-write" | "danger-full-access";

export const SANDBOX_TIERS: readonly SandboxTier[] = [
  "read-only",
  "workspace-write",
  "danger-full-access",
] as const;

/** Default matches the Rust default — historic behavior is workspace-write. */
export const DEFAULT_SANDBOX_TIER: SandboxTier = "workspace-write";

function isTier(s: string): s is SandboxTier {
  return (SANDBOX_TIERS as readonly string[]).includes(s);
}

/** Read the configured sandbox tier for `projectRoot`. Returns the default
 *  when no `.cortex/sandbox.toml` exists or it fails to parse. */
export async function getSandboxTier(projectRoot: string): Promise<SandboxTier> {
  const raw = await invoke<string>("get_sandbox_tier", { projectRoot });
  return isTier(raw) ? raw : DEFAULT_SANDBOX_TIER;
}

/** Persist the sandbox tier for `projectRoot`. Rejects on invalid tier
 *  strings; the Rust side does its own validation too. */
export async function setSandboxTier(
  projectRoot: string,
  tier: SandboxTier,
): Promise<void> {
  if (!isTier(tier)) {
    throw new Error(`invalid sandbox tier '${tier}'`);
  }
  return invoke("set_sandbox_tier", { projectRoot, tier });
}

/** Classification of a shell command line. Mirrors `CommandClassification`
 *  in `src-tauri/src/commands/sandbox.rs`. A `readOnly:true` command (Codex's
 *  `is_safe_command`) is safe to run under any tier — including `read-only`,
 *  which is what an untrusted project is forced into. */
export interface CommandClassification {
  readOnly: boolean;
  reason: string;
}

/** Ask the backend whether a shell command is provably read-only (safe to run
 *  under the read-only sandbox tier). Use to label `/run` input or an approval
 *  prompt. */
export async function classifyShellCommand(
  command: string,
): Promise<CommandClassification> {
  const r = await invoke<{ read_only: boolean; reason: string }>(
    "classify_shell_command",
    { command },
  );
  return { readOnly: r.read_only, reason: r.reason };
}

/** Human label + short description, for use in popovers and settings. */
export const SANDBOX_TIER_META: Record<
  SandboxTier,
  { label: string; description: string; color: string }
> = {
  "read-only": {
    label: "Read-only",
    description:
      "Read tools + provably read-only shell commands (git status, ls, grep…).",
    color: "var(--sandbox-readonly, #3b82f6)",
  },
  "workspace-write": {
    label: "Workspace write",
    description: "Read + write/edit/patch/run inside the project root.",
    color: "var(--sandbox-workspace, #22c55e)",
  },
  "danger-full-access": {
    label: "Danger: full access",
    description: "No tier gate. Guardrails still apply.",
    color: "var(--sandbox-danger, #ff6b6b)",
  },
};
