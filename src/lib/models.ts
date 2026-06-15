import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * One selectable model in the composer picker. Aggregated backend-side from the
 * local Claude Code CLI (`source: "claude-cli"`) and the Cortex Gateway
 * (`source: "gateway"`). See `commands/models.rs`.
 */
export interface ModelEntry {
  id: string;
  label: string;
  source: string;
  available: boolean;
}

/** Aggregate model list (Claude CLI + Cortex Gateway). Best-effort: returns [] on failure. */
export async function listModels(): Promise<ModelEntry[]> {
  return invoke<ModelEntry[]>("list_models");
}

/**
 * Subscribe to backend `models:changed` events (emitted when the model
 * universe shifts — e.g. a Cookbook pull lands a new Ollama tag) so pickers
 * can refresh without a restart. Returns an unlisten fn.
 */
export async function onModelsChanged(cb: () => void): Promise<UnlistenFn> {
  try {
    return await listen("models:changed", () => cb());
  } catch {
    // Non-Tauri context (vite-in-browser e2e shots) — no events, no-op unlisten.
    return () => {};
  }
}

/**
 * Display metadata per model `source`. `label` is the human-facing group name;
 * `hue` is the design-token hue class applied to that group's pills. Unknown
 * sources fall back to a neutral label/hue (see `sourceMeta`).
 */
export const SOURCE_META: Record<string, { label: string; hue: string }> = {
  "claude-cli": { label: "Claude", hue: "hue-blue" },
  gateway: { label: "Cortex Gateway", hue: "hue-purple" },
  ollama: { label: "Ollama", hue: "hue-green" },
};

/** Stable display order for source groups; unknown sources sort last. */
const SOURCE_ORDER: string[] = ["claude-cli", "gateway", "ollama"];

/** Resolve display metadata for a source, falling back for unknown sources. */
export function sourceMeta(source: string): { label: string; hue: string } {
  return SOURCE_META[source] ?? { label: source, hue: "hue-blue" };
}

/**
 * Group a flat model list by `source`, preserving original entry order within
 * each group and emitting groups in `SOURCE_ORDER` (unknown sources appended
 * alphabetically). Pure — covered by unit tests.
 */
export function groupModelsBySource(
  models: ModelEntry[],
): Array<{ source: string; models: ModelEntry[] }> {
  const map = new Map<string, ModelEntry[]>();
  for (const m of models) {
    const bucket = map.get(m.source);
    if (bucket) bucket.push(m);
    else map.set(m.source, [m]);
  }
  const sources = [...map.keys()].sort((a, b) => {
    const ia = SOURCE_ORDER.indexOf(a);
    const ib = SOURCE_ORDER.indexOf(b);
    if (ia !== -1 && ib !== -1) return ia - ib;
    if (ia !== -1) return -1;
    if (ib !== -1) return 1;
    return a.localeCompare(b);
  });
  return sources.map((source) => ({ source, models: map.get(source)! }));
}
