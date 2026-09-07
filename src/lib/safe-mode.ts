import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { subscribeConfigChanges } from "@/lib/config-watcher";
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * Safe Mode (issue 004) bindings. Mirrors `SafeMode` /
 * `PolicyDecision` in `src-tauri/src/commands/safe_mode.rs` and
 * `src-tauri/src/orchestrator/command_policy.rs`.
 *
 * Safe Mode is DEFAULT-OFF (`~/.cortex/safe-mode.json` absent = off). When
 * on, the backend clamps the sandbox tier (`danger-full-access` behaves as
 * `workspace-write`), pulls the approval policy back from `never`, enforces
 * the command allow/denylist before the guardrails, suppresses auto-approval
 * of non-allowlisted shell commands, and audits every tool call.
 */
export interface SafeMode {
  enabled: boolean;
  /** Unix epoch millis of the last enable; null when disabled. */
  enabled_at: number | null;
  /**
   * Full scope (issue 004): set by the built-in "CI-safe" preset. When true,
   * the backend additionally clamps the effective sandbox tier all the way
   * to `read-only` (narrow-only — stricter than the plain
   * `danger-full-access → workspace-write` clamp).
   */
  force_read_only: boolean;
}

export type PolicyAction = "allow" | "deny" | "ask";

/** Dry-run result: which rule matched and from which file. Never executes. */
export interface PolicyDecision {
  action: PolicyAction;
  /** Pattern text of the matched rule; null for a default decision. */
  matched: string | null;
  reason: string | null;
  /** "global" | "project" | "default". */
  source: string;
}

/** Result of `export_audit_log`: the written file + row count. */
export interface AuditLogExport {
  path: string;
  rows: number;
}

export async function safeModeStatus(): Promise<SafeMode> {
  return invoke<SafeMode>("safe_mode_status");
}

/** Toggle Safe Mode. The toggle itself is written to the audit log. */
export async function setSafeMode(enabled: boolean): Promise<SafeMode> {
  return invoke<SafeMode>("set_safe_mode", { enabled });
}

/**
 * Raw TOML of a command-policy file for the editor. Omit `projectRoot` for
 * the global `~/.cortex/command-policy.toml`. A missing file yields a
 * commented starter template.
 */
export async function getCommandPolicy(projectRoot?: string | null): Promise<string> {
  return invoke<string>("get_command_policy", { projectRoot: projectRoot ?? null });
}

/** Validate-then-write a policy file. Rejects (file untouched) on bad TOML. */
export async function setCommandPolicy(
  projectRoot: string | null,
  raw: string,
): Promise<void> {
  return invoke("set_command_policy", { projectRoot, raw });
}

/** Dry-run a command against the effective global+project policy. */
export async function testCommandPolicy(
  projectRoot: string | null,
  command: string,
): Promise<PolicyDecision> {
  return invoke<PolicyDecision>("test_command_policy", { projectRoot, command });
}

/**
 * Apply the built-in "CI-safe" preset (issue 004 full scope): max lockdown —
 * enables Safe Mode with the sandbox tier clamped to read-only, OVERWRITES
 * the global command policy with allowlist mode (`default_ask = true`) plus
 * the built-in destructive-command heuristics, and (for free, since it's
 * already true whenever Safe Mode is on) audits every tool call. Project
 * policy files are untouched. Show `ciSafePolicyPreview` to the user before
 * calling this, since it replaces their existing global policy file.
 */
export async function applyCiSafeProfile(): Promise<SafeMode> {
  return invoke<SafeMode>("apply_ci_safe_profile");
}

/**
 * The exact global command-policy body `applyCiSafeProfile` is about to
 * write — a preview so the UI can show what "max lockdown" means before the
 * user confirms overwriting their global policy file.
 */
export async function ciSafePolicyPreview(): Promise<string> {
  return invoke<string>("ci_safe_policy_preview");
}

/**
 * The built-in destructive-command heuristics (rm -rf, dd, mkfs, fork
 * bombs, git push --force, curl|sh installers, chmod -R 777, …) as TOML
 * text, for a read-only "view built-in rules" panel. These are ALWAYS
 * enforced while Safe Mode's command policy is active regardless of the
 * user's own global/project files (which can only narrow further, never
 * loosen a built-in Deny/Ask back to Allow) — this is what the user can
 * copy into their own editable policy file to customize wording or add
 * narrower rules of their own.
 */
export async function getBuiltinCommandRules(): Promise<string> {
  return invoke<string>("get_builtin_command_rules");
}

/**
 * Export the audit log (redacted through the backend choke point) to
 * `~/.cortex/audit-export-<ts>.<jsonl|csv>` and return its path.
 */
export async function exportAuditLog(
  format: "jsonl" | "csv",
  fromTs?: number,
  toTs?: number,
): Promise<AuditLogExport> {
  return invoke<AuditLogExport>("export_audit_log", {
    fromTs: fromTs ?? null,
    toTs: toTs ?? null,
    format,
  });
}

/**
 * Live Safe Mode flag for badges. Reads the backend on mount, refreshes on
 * `config-changed` events for `safe-mode.json` (the `~/.cortex` hot-reload
 * watcher), and polls slowly as a fallback. Backend is the source of truth —
 * no local store.
 */
export function useSafeMode(): boolean {
  const [enabled, setEnabled] = useState(false);
  useEffect(() => {
    let mounted = true;
    let off: UnlistenFn | undefined;
    const refresh = async () => {
      try {
        const s = await safeModeStatus();
        if (mounted) setEnabled(s.enabled);
      } catch {
        /* backend warming — keep last known value */
      }
    };
    void refresh();
    void subscribeConfigChanges((evt) => {
      if (evt.path.endsWith("safe-mode.json")) void refresh();
    }).then((fn) => {
      off = fn;
    });
    const id = setInterval(refresh, 30_000);
    return () => {
      mounted = false;
      off?.();
      clearInterval(id);
    };
  }, []);
  return enabled;
}
