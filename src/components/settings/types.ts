import type { ReactNode } from "react";

/**
 * Settings modal registry types. Each tab module under `settings/` exports a
 * list of {@link SectionDef}s; `SettingsModal` concatenates them, filters by
 * the search box (heading + `text` keyword index) and renders the active tab.
 */

export type TabId =
  | "general"
  | "connections"
  | "providers"
  | "workspace"
  | "theme"
  | "updates"
  | "advanced";

export const TABS: { id: TabId; label: string }[] = [
  { id: "general", label: "General" },
  { id: "connections", label: "Connections" },
  { id: "providers", label: "Providers" },
  { id: "workspace", label: "Workspace" },
  { id: "theme", label: "Theme" },
  { id: "updates", label: "Updates" },
  { id: "advanced", label: "Advanced" },
];

const TAB_STORAGE_KEY = "cortex.settingsTab";

export function loadTab(): TabId {
  try {
    const raw = localStorage.getItem(TAB_STORAGE_KEY);
    if (raw && TABS.some((t) => t.id === raw)) return raw as TabId;
  } catch {
    /* ignore */
  }
  return "general";
}

export function persistTab(id: TabId) {
  try {
    localStorage.setItem(TAB_STORAGE_KEY, id);
  } catch {
    /* ignore */
  }
}

/**
 * The gateway/Ollama/Obsidian form is owned by the modal because the footer
 * "Save" button commits all of it at once. Sections that edit those fields
 * receive it through {@link SettingsCtx}; everything else owns its own state.
 */
export interface GatewayForm {
  baseUrl: string;
  setBaseUrl: (v: string) => void;
  model: string;
  setModel: (v: string) => void;
  apiKey: string;
  setApiKey: (v: string) => void;
  hasKey: boolean;
  ollamaUrl: string;
  setOllamaUrl: (v: string) => void;
  ollamaModel: string;
  setOllamaModel: (v: string) => void;
  obsidian: string;
  setObsidian: (v: string) => void;
  /** True when any field differs from what was loaded (footer Save pending). */
  dirty: boolean;
}

export interface SettingsCtx {
  gateway: GatewayForm;
}

// One section per "card" inside a tab. `text` is concatenated heading + body
// keywords used for the substring search filter at the top of the nav.
export interface SectionDef {
  tab: TabId;
  heading: string;
  text: string;
  render: (ctx: SettingsCtx) => ReactNode;
}
