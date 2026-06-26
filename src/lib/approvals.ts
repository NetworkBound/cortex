import { invoke } from "@tauri-apps/api/core";

export type ApprovalRuleDecision = "approve" | "deny";

/**
 * Codex-style **approval policy** — the orthogonal "when do we pause to ask the
 * user?" axis that sits alongside the sandbox tier. Mirrors `ApprovalPolicy` in
 * `src-tauri/src/orchestrator/approval_policy.rs`.
 *
 *  - `untrusted`  — ask for everything except provably read-only inspection
 *                   (read tools + read-only shell commands).
 *  - `on-request` — DEFAULT. Forward every approval request to the user.
 *  - `never`      — never pause; auto-approve anything that already passed the
 *                   sandbox tier + guardrails ("full-auto within the sandbox").
 *
 * The policy only ever *skips* a prompt — it cannot widen what the sandbox tier
 * or guardrails already forbid (those gates run first in `chat.rs`).
 */
export type ApprovalPolicy = "untrusted" | "on-request" | "never";

export const APPROVAL_POLICIES: readonly ApprovalPolicy[] = [
  "untrusted",
  "on-request",
  "never",
] as const;

/** Default matches the Rust default — historic behavior (always ask). */
export const DEFAULT_APPROVAL_POLICY: ApprovalPolicy = "on-request";

function isPolicy(s: string): s is ApprovalPolicy {
  return (APPROVAL_POLICIES as readonly string[]).includes(s);
}

/** Human label + short description, for use in popovers and settings. */
export const APPROVAL_POLICY_META: Record<
  ApprovalPolicy,
  { label: string; description: string }
> = {
  untrusted: {
    label: "Untrusted",
    description:
      "Ask before anything except provably read-only inspection (git status, ls, grep…).",
  },
  "on-request": {
    label: "On request",
    description: "Ask for every tool call that needs approval (default).",
  },
  never: {
    label: "Never ask",
    description:
      "Auto-approve everything the sandbox tier + guardrails already allow.",
  },
};

/** Read the configured approval policy for `projectRoot`. Returns the default
 *  when no `.cortex/approval-policy.toml` exists or it fails to parse. */
export async function getApprovalPolicy(
  projectRoot: string,
): Promise<ApprovalPolicy> {
  const raw = await invoke<string>("get_approval_policy", { projectRoot });
  return isPolicy(raw) ? raw : DEFAULT_APPROVAL_POLICY;
}

/** Persist the approval policy for `projectRoot`. The Rust side validates too. */
export async function setApprovalPolicy(
  projectRoot: string,
  policy: ApprovalPolicy,
): Promise<void> {
  if (!isPolicy(policy)) {
    throw new Error(`invalid approval policy '${policy}'`);
  }
  return invoke("set_approval_policy", { projectRoot, policy });
}

/**
 * Append a persistent approval rule to `<projectRoot>/.cortex/approvals.toml`.
 * The backend validates `pattern` as a regex; an invalid regex bubbles up as
 * a rejected promise.
 */
export async function addApprovalRule(
  projectRoot: string,
  pattern: string,
  decision: ApprovalRuleDecision,
): Promise<void> {
  return invoke("add_approval_rule", {
    projectRoot,
    pattern,
    decision,
  });
}

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
