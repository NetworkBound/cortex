// Workspace layout presets — capture and restore the small set of UI toggles
// that together define a "where am I working?" mood.
//
// A preset bundles:
//   - active ActivityPanel tab (left sidebar selection)
//   - current Cortex mode (`plan` / `act`)
//   - sandbox tier for the active project (`read-only` | `workspace-write` | `danger-full-access`)
//   - current theme (one of `ThemeId`)
//   - active gateway model (free-form string)
//   - right-column sidebar tab (`memory` | `brain` | `agent`) — best-effort,
//     since this lives in App.tsx local state. We persist a localStorage hint
//     + dispatch a `cortex:right-tab` event so a future App.tsx listener can
//     pick it up without us editing that file in this change.
//
// Backend persistence lives in `commands/workspace_presets.rs`. All helpers
// here are best-effort: a missing/corrupt presets file degrades to "no
// presets yet" rather than throwing, mirroring `lib/snippets.ts`.

import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { getGatewayConfig, updateGatewayConfig } from "@/lib/cortex-bridge";
import {
  DEFAULT_SANDBOX_TIER,
  SANDBOX_TIERS,
  getSandboxTier,
  setSandboxTier,
  type SandboxTier,
} from "@/lib/sandbox";
import { applyTheme, loadTheme, THEMES, type ThemeId } from "@/lib/themes";
import { useCortexStore, type ActivityTab, type Mode } from "@/state/store";

export type RightTab = "memory" | "brain" | "agent";

/** localStorage key + custom event used to communicate a restored right-tab
 *  back to App.tsx without editing that file in this change. */
export const RIGHT_TAB_STORAGE_KEY = "cortex.rightTab";
export const RIGHT_TAB_EVENT = "cortex:right-tab";

export interface WorkspacePresetState {
  activity_tab: string | null;
  mode: string | null;
  sandbox_tier: string | null;
  theme: string | null;
  gateway_model: string | null;
  /** Back-compat: presets saved before the Cortex Gateway rebrand stored the
   *  active model under `hermes_model`. Still read on apply (see below) so
   *  existing saved presets keep working with zero migration. */
  hermes_model?: string | null;
  right_tab: string | null;
}

export interface WorkspacePreset {
  name: string;
  description: string;
  state: WorkspacePresetState;
  created_unix_ms: number;
}

const RIGHT_TABS: RightTab[] = ["memory", "brain", "agent"];

function isRightTab(v: unknown): v is RightTab {
  return typeof v === "string" && (RIGHT_TABS as string[]).includes(v);
}

function isThemeId(v: unknown): v is ThemeId {
  return typeof v === "string" && THEMES.some((t) => t.id === v);
}

function isMode(v: unknown): v is Mode {
  return v === "plan" || v === "act";
}

function isSandboxTier(v: unknown): v is SandboxTier {
  return typeof v === "string" && (SANDBOX_TIERS as readonly string[]).includes(v);
}

/** Read whatever the right-column sidebar last set in localStorage. Returns
 *  `null` when nothing has been persisted yet — callers should fall back to
 *  the App.tsx default ("memory"). */
function readRightTab(): RightTab | null {
  try {
    const raw = localStorage.getItem(RIGHT_TAB_STORAGE_KEY);
    return isRightTab(raw) ? raw : null;
  } catch {
    return null;
  }
}

/** Persist + broadcast a right-tab switch. App.tsx doesn't subscribe yet (we
 *  intentionally don't touch it in this change), but it can opt in later by
 *  listening for `RIGHT_TAB_EVENT` and seeding state from `localStorage`. */
function applyRightTab(tab: RightTab): void {
  try {
    localStorage.setItem(RIGHT_TAB_STORAGE_KEY, tab);
  } catch {
    /* ignore */
  }
  try {
    window.dispatchEvent(new CustomEvent(RIGHT_TAB_EVENT, { detail: { tab } }));
  } catch {
    /* not in browser env — ignore */
  }
}

export async function listWorkspacePresets(): Promise<WorkspacePreset[]> {
  try {
    return await invoke<WorkspacePreset[]>("list_workspace_presets");
  } catch {
    return [];
  }
}

export async function deleteWorkspacePreset(name: string): Promise<boolean> {
  try {
    await invoke("delete_workspace_preset", { name });
    return true;
  } catch (err) {
    console.warn("deleteWorkspacePreset failed", err);
    return false;
  }
}

async function saveWorkspacePresetRaw(
  preset: WorkspacePreset,
): Promise<WorkspacePreset | null> {
  try {
    return await invoke<WorkspacePreset>("save_workspace_preset", { preset });
  } catch (err) {
    console.warn("saveWorkspacePreset failed", err);
    return null;
  }
}

/** Capture the current workspace into a named preset. Pulls every field from
 *  the live store / DOM so a user can `Save current as preset` without
 *  filling out a form. */
export async function savePresetFromCurrentState(
  name: string,
  description: string,
): Promise<WorkspacePreset | null> {
  const trimmedName = name.trim();
  if (!trimmedName) {
    throw new Error("preset name is required");
  }
  const state = useCortexStore.getState();
  const project = state.activeProject;

  let sandboxTier: SandboxTier | null = null;
  if (project) {
    try {
      sandboxTier = await getSandboxTier(project.root);
    } catch {
      sandboxTier = null;
    }
  }

  let gatewayModel: string | null = null;
  try {
    const cfg = await getGatewayConfig();
    gatewayModel = cfg.model || null;
  } catch {
    gatewayModel = null;
  }

  const preset: WorkspacePreset = {
    name: trimmedName,
    description: description.trim(),
    state: {
      activity_tab: state.activityTab,
      mode: state.currentMode,
      sandbox_tier: sandboxTier,
      theme: loadTheme(),
      gateway_model: gatewayModel,
      right_tab: readRightTab(),
    },
    created_unix_ms: Date.now(),
  };

  return saveWorkspacePresetRaw(preset);
}

/** Look up a preset by name (case-sensitive, post-trim). Returns null when
 *  there's no match — callers surface this as a toast. */
export async function getPreset(name: string): Promise<WorkspacePreset | null> {
  const needle = name.trim();
  if (!needle) return null;
  const all = await listWorkspacePresets();
  return all.find((p) => p.name === needle) ?? null;
}

/** Result of `applyPreset`: which fields actually got applied (the rest were
 *  null / invalid / unavailable). Surfaced by the modal + slash command so
 *  the user understands what just changed. */
export interface ApplyReport {
  applied: string[];
  skipped: string[];
}

/** Restore every state field present in `preset` in one shot. Best-effort:
 *  individual failures are recorded in `skipped` but never throw, so a missing
 *  active project doesn't kill the rest of the restore. */
export async function applyPresetState(
  preset: WorkspacePreset,
): Promise<ApplyReport> {
  const applied: string[] = [];
  const skipped: string[] = [];
  const store = useCortexStore.getState();
  const s = preset.state;

  // Activity tab — the backend always populates this field, using null for a
  // collapsed sidebar. We only restore (and report) it when an actual tab name
  // is present; a null/absent value is a no-op.
  if (s.activity_tab != null) {
    // ActivityTab is a string-or-null union; we accept any string the store
    // currently knows about or `null`. Unknown strings are stored anyway so
    // forward-compat with new tabs Just Works.
    store.setActivityTab(s.activity_tab as ActivityTab);
    applied.push("activity tab");
  }

  if (isMode(s.mode)) {
    store.setCurrentMode(s.mode);
    applied.push("mode");
  } else if (s.mode) {
    skipped.push(`mode (unknown: ${s.mode})`);
  }

  if (s.sandbox_tier) {
    if (!isSandboxTier(s.sandbox_tier)) {
      skipped.push(`sandbox tier (unknown: ${s.sandbox_tier})`);
    } else {
      const project = store.activeProject;
      if (!project) {
        skipped.push("sandbox tier (no active project)");
      } else {
        try {
          await setSandboxTier(project.root, s.sandbox_tier as SandboxTier);
          applied.push("sandbox tier");
        } catch (err) {
          skipped.push(`sandbox tier (${humanizeError(err)})`);
        }
      }
    }
  }

  if (s.theme) {
    if (isThemeId(s.theme)) {
      applyTheme(s.theme);
      applied.push("theme");
    } else {
      skipped.push(`theme (unknown: ${s.theme})`);
    }
  }

  // Back-compat: fall back to the legacy `hermes_model` key for presets saved
  // before the rebrand.
  const presetModel = s.gateway_model ?? s.hermes_model ?? null;
  if (presetModel) {
    try {
      await updateGatewayConfig({ gateway_model: presetModel });
      applied.push("gateway model");
    } catch (err) {
      skipped.push(`gateway model (${humanizeError(err)})`);
    }
  }

  if (s.right_tab) {
    if (isRightTab(s.right_tab)) {
      applyRightTab(s.right_tab);
      applied.push("right tab");
    } else {
      skipped.push(`right tab (unknown: ${s.right_tab})`);
    }
  }

  return { applied, skipped };
}

/** Fetch + apply by name in one shot. Returns `null` when no preset matches —
 *  used by the `/preset <name>` slash command. */
export async function applyPreset(name: string): Promise<ApplyReport | null> {
  const preset = await getPreset(name);
  if (!preset) return null;
  return applyPresetState(preset);
}

/** Defaults used by the sandbox badge when a preset doesn't capture a tier. */
export const SANDBOX_TIER_FALLBACK: SandboxTier = DEFAULT_SANDBOX_TIER;
