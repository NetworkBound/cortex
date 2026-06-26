import { createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  QuickOpenModal,
  type QuickOpenModalProps,
  type QuickOpenPick,
} from "@/components/QuickOpenModal";
import { readConfigFile, writeConfigFile, type ConfigTarget } from "@/lib/config-files";
import { openInEditor } from "@/lib/editor";

/**
 * Recent-files quick-open helpers.
 *
 * - `openQuickOpen(initial?)` self-mounts the `<QuickOpenModal />` on
 *   `document.body` (mirrors the IDEExportModal portal pattern so the
 *   slash command can summon it without App.tsx wiring).
 * - `loadRecentFiles()` / `recordRecentFile()` persist an MRU list to
 *   `~/.cortex/recent-files.json` via the existing `read_config_file` /
 *   `write_config_file` Tauri commands.
 * - `pickFile(path)` is the canonical "user picked a file in the picker"
 *   action: records it as recent, then broadcasts `cortex:editor-open`.
 */

export type { QuickOpenPick } from "@/components/QuickOpenModal";

export const RECENT_FILES_PATH = "recent-files.json";
export const RECENT_FILES_MAX = 50;

const RECENT_TARGET: ConfigTarget = {
  scope: "home",
  rel_path: RECENT_FILES_PATH,
};

export interface RecentFile {
  path: string;
  accessed_unix_ms: number;
}

/** Tolerantly parse `recent-files.json`. Returns [] on any structural issue. */
function parseRecents(body: string): RecentFile[] {
  if (!body.trim()) return [];
  try {
    const raw = JSON.parse(body);
    if (!Array.isArray(raw)) return [];
    const out: RecentFile[] = [];
    for (const entry of raw) {
      if (!entry || typeof entry !== "object") continue;
      const path = (entry as { path?: unknown }).path;
      const ts = (entry as { accessed_unix_ms?: unknown }).accessed_unix_ms;
      if (typeof path !== "string" || !path) continue;
      const accessed = typeof ts === "number" && Number.isFinite(ts) ? ts : 0;
      out.push({ path, accessed_unix_ms: accessed });
    }
    return out;
  } catch {
    return [];
  }
}

/** Read the MRU list. Missing file / parse errors all collapse to `[]`. */
export async function loadRecentFiles(): Promise<RecentFile[]> {
  try {
    const result = await readConfigFile(RECENT_TARGET);
    if (!result.exists) return [];
    return parseRecents(result.body);
  } catch {
    return [];
  }
}

/**
 * Push `path` to the front of the MRU list and persist. Existing entries
 * for the same path are de-duped (we keep only the newest accessed_unix_ms).
 * The list is capped at `RECENT_FILES_MAX` so the file stays bounded.
 */
export async function recordRecentFile(path: string): Promise<RecentFile[]> {
  const trimmed = path?.trim?.() ?? "";
  if (!trimmed) return [];
  const current = await loadRecentFiles();
  const filtered = current.filter((r) => r.path !== trimmed);
  const next: RecentFile[] = [
    { path: trimmed, accessed_unix_ms: Date.now() },
    ...filtered,
  ].slice(0, RECENT_FILES_MAX);
  try {
    await writeConfigFile(RECENT_TARGET, JSON.stringify(next, null, 2));
  } catch {
    /* best-effort — even if persistence fails we still want to open */
  }
  return next;
}

/**
 * Canonical "user picked a file in the quick-open picker" action.
 * Records it to recents (fire-and-forget) and asks the editor pane to
 * open it via the existing `cortex:editor-open` event.
 */
export function pickFile(path: string): void {
  const trimmed = path?.trim?.() ?? "";
  if (!trimmed) return;
  void recordRecentFile(trimmed);
  openInEditor(trimmed);
}

/**
 * Imperative summoners. Each mounts a fresh `<QuickOpenModal />` on a
 * detached `<div>` and tears it down on close. Re-entrant guard — summoning
 * while one is already open is a no-op.
 */
let activeRoot: Root | null = null;

/** Mount the modal with `props` (minus onClose, which we own). Returns false
 *  when another instance is already open. `onClosed` fires exactly once on
 *  teardown, whatever the close path. */
function mountQuickOpenModal(
  props: Omit<QuickOpenModalProps, "onClose">,
  onClosed?: () => void,
): boolean {
  if (activeRoot) return false;
  const container = document.createElement("div");
  container.dataset.cortexMount = "quick-open";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
    onClosed?.();
  };
  try {
    root.render(createElement(QuickOpenModal, { ...props, onClose: close }));
    return true;
  } catch (err) {
    // If the initial render throws, tear down so the guard can't wedge
    // permanently (otherwise `activeRoot` stays set and every future
    // summon becomes a no-op).
    close();
    throw err;
  }
}

/** Used by the `/open` slash command: pick a file, open it in the editor. */
export function openQuickOpen(initialQuery?: string): void {
  mountQuickOpenModal({ initialQuery: initialQuery ?? "" });
}

export interface PickFileOptions {
  initialQuery?: string;
  /** Accessible label for the dialog (e.g. "Add excerpt"). */
  title?: string;
  /** Show the inline line-range field (default true). */
  withRange?: boolean;
}

/**
 * Promise-based picker for surfaces that need a file *reference* rather
 * than "open it in the editor" — e.g. the multibuffer's "+ Add excerpt".
 * Resolves with the picked path + parsed line range (`range: null` = whole
 * file), or `null` if the user cancelled (Escape / backdrop) or another
 * quick-open instance is already on screen.
 */
export function pickFileWithRange(
  opts: PickFileOptions = {},
): Promise<QuickOpenPick | null> {
  return new Promise((resolve) => {
    let picked: QuickOpenPick | null = null;
    const mounted = mountQuickOpenModal(
      {
        initialQuery: opts.initialQuery ?? "",
        title: opts.title,
        withRange: opts.withRange ?? true,
        onPick: (pick) => {
          picked = pick;
        },
      },
      // Resolve on teardown (the modal closes itself right after onPick),
      // so every close path — pick, Escape, backdrop — settles the promise.
      () => resolve(picked),
    );
    if (!mounted) resolve(null);
  });
}
