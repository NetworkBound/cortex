/**
 * Gather/Agent mode toggle (Void-style two-mode switch).
 *
 * - **Gather** is read-only: auto-approve reads inside the workspace, but
 *   every write tool and every shell command still has to round-trip through
 *   the standard approval prompt. Good for "let me browse the project
 *   without worrying I'll wipe a file".
 * - **Agent** is the full write+exec policy the user already had set up. We
 *   snapshot whatever the trust matrix looked like when the user FIRST
 *   switches to Gather, so flipping back restores their custom Agent
 *   profile rather than clobbering it with defaults.
 *
 * Persistence:
 * - The current mode lives in `localStorage` under `cortex.gather-mode`.
 *   This is intentionally separate from the trust matrix on disk — the
 *   trust matrix is "what the policy is right now", this flag is "which
 *   preset is selected".
 * - The previous Agent matrix is cached at `cortex.gather-mode.agent-snapshot`
 *   so we can restore it verbatim when the user flips back.
 *
 * Source of truth: this module owns ZERO state itself. It reads/writes
 * via the existing `get_trust_matrix` / `set_trust_matrix` Tauri commands
 * (see commands/trust.rs) so the approval pipeline transparently picks up
 * the change without any extra wiring.
 */

import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";

export type GatherMode = "gather" | "agent";

const MODE_KEY = "cortex.gather-mode";
const SNAPSHOT_KEY = "cortex.gather-mode.agent-snapshot";

export interface TrustMatrixShape {
  read_in_workspace: boolean;
  read_outside: boolean;
  edit_in_workspace: boolean;
  edit_outside: boolean;
  safe_commands: boolean;
  all_commands: boolean;
  browser: boolean;
  mcp: boolean;
  max_requests_per_task: number;
}

/** Read-only profile used while in Gather mode. Reads inside the workspace
 *  are auto-approved; literally everything else has to ask. */
export const GATHER_PROFILE: TrustMatrixShape = {
  read_in_workspace: true,
  read_outside: false,
  edit_in_workspace: false,
  edit_outside: false,
  safe_commands: false,
  all_commands: false,
  browser: false,
  mcp: false,
  max_requests_per_task: 20,
};

/** Returns the persisted mode, defaulting to "agent" so existing users see
 *  no behaviour change until they explicitly flip the switch. */
export function getMode(): GatherMode {
  try {
    const raw = localStorage.getItem(MODE_KEY);
    return raw === "gather" ? "gather" : "agent";
  } catch {
    return "agent";
  }
}

function persistMode(mode: GatherMode) {
  try {
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    /* private mode / quota — best-effort */
  }
}

function snapshotAgent(matrix: TrustMatrixShape) {
  try {
    localStorage.setItem(SNAPSHOT_KEY, JSON.stringify(matrix));
  } catch {
    /* best-effort */
  }
}

function loadAgentSnapshot(): TrustMatrixShape | null {
  try {
    const raw = localStorage.getItem(SNAPSHOT_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    // Defensive: pad missing keys with `false`/`20` rather than reject the
    // whole snapshot — a partial restore is still better than the defaults.
    return {
      read_in_workspace: Boolean(parsed.read_in_workspace),
      read_outside: Boolean(parsed.read_outside),
      edit_in_workspace: Boolean(parsed.edit_in_workspace),
      edit_outside: Boolean(parsed.edit_outside),
      safe_commands: Boolean(parsed.safe_commands),
      all_commands: Boolean(parsed.all_commands),
      browser: Boolean(parsed.browser),
      mcp: Boolean(parsed.mcp),
      max_requests_per_task:
        typeof parsed.max_requests_per_task === "number"
          ? parsed.max_requests_per_task
          : 20,
    };
  } catch {
    return null;
  }
}

/**
 * Flip the toggle. Reads the current trust matrix, snapshots it (if we're
 * leaving Agent), writes the new policy through `set_trust_matrix`, then
 * fires a confirmation toast.
 */
export async function setMode(next: GatherMode): Promise<void> {
  // Pull whatever the matrix is right now so we can either snapshot it
  // (going gather) or compare against it (going agent).
  let current: TrustMatrixShape | null = null;
  try {
    current = await invoke<TrustMatrixShape>("get_trust_matrix");
  } catch (err) {
    // If the backend can't load it, fall back to a safe default — the user
    // will still see the toggle flip but no policy gets clobbered.
    console.warn("get_trust_matrix failed", err);
  }

  if (next === "gather") {
    // Only snapshot when we're actually LEAVING Agent mode. If we're already
    // in Gather, `current` is the read-only GATHER_PROFILE — snapshotting it
    // would clobber the user's saved Agent trust matrix and lose it forever.
    if (current && getMode() !== "gather") snapshotAgent(current);
    try {
      await invoke("set_trust_matrix", { matrix: GATHER_PROFILE });
    } catch (err) {
      pushToast({
        title: "Gather mode failed",
        body: humanizeError(err),
        kind: "error",
      });
      return;
    }
    persistMode("gather");
    pushToast({
      title: "🔍 Gather mode",
      body: "Read-only — write tools and shell commands will ask first.",
      kind: "info",
    });
    return;
  }

  // next === "agent" — restore the snapshot if we have one, otherwise just
  // mark the mode and leave whatever the user currently has on disk alone.
  const snapshot = loadAgentSnapshot();
  if (snapshot) {
    try {
      await invoke("set_trust_matrix", { matrix: snapshot });
    } catch (err) {
      pushToast({
        title: "Agent mode failed",
        body: humanizeError(err),
        kind: "error",
      });
      return;
    }
  }
  persistMode("agent");
  pushToast({
    title: "⚡ Agent mode",
    body: snapshot
      ? "Restored your previous trust profile."
      : "Standard trust profile active.",
    kind: "success",
  });
}

/** Convenience: flip whichever direction makes sense given the current
 *  persisted mode. Used by the `/gather` slash command. */
export async function toggleMode(): Promise<GatherMode> {
  const next: GatherMode = getMode() === "agent" ? "gather" : "agent";
  await setMode(next);
  return next;
}
