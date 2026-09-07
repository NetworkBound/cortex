/**
 * Zed-style multibuffer — excerpt state + save logic.
 *
 * A "multibuffer" stitches together N editable excerpts from N files into
 * a single tab. Each excerpt carries its own dirty flag and is saved
 * back into the source file by reading the live disk contents and
 * splicing the new body into the `[start_line, end_line]` window.
 *
 * The state itself lives in the Zustand store (`multibufferExcerpts`).
 * This file is the pure-logic side: language detection, splice math,
 * IO via the existing `save_file_text` / `read_file_text` Tauri
 * commands, and a small `cortex:multibuffer-open` event for callers
 * (search/refactor agents) that want to bulk-populate the tab.
 */
import { invoke } from "@tauri-apps/api/core";
import { readTextFile } from "@tauri-apps/plugin-fs";

import { extOf, languageLabel } from "@/lib/editor-langs";
import { saveFileText } from "@/lib/editor-save";
import { useCortexStore } from "@/state/store";

/** A single editable slice of a source file, displayed in the multibuffer. */
export interface MultibufferExcerpt {
  /** Stable id — `mb-<random>`. Used as the React key + dirty-bit map key. */
  id: string;
  /** Absolute path on disk. */
  path: string;
  /** Best-guess language label (driven by the file extension). */
  language: string;
  /** 1-based inclusive line numbers. */
  start_line: number;
  end_line: number;
  /** Current excerpt body. Edited inline; written back on save. */
  body: string;
  /** True when the live body differs from what was last loaded/saved. */
  dirty: boolean;
}

/** Window event name that callers fire to replace the multibuffer. */
export const MULTIBUFFER_OPEN_EVENT = "cortex:multibuffer-open";

export interface MultibufferOpenDetail {
  excerpts: MultibufferExcerpt[];
}

/** Mint a fresh id. `mb-` prefix keeps it visually distinct from message ids. */
export function newExcerptId(): string {
  return `mb-${crypto.randomUUID()}`;
}

/** Resolve the language label for a given source path. Thin wrapper so the
 *  React component doesn't need to import editor-langs directly. */
export function languageFor(path: string): string {
  return languageLabel(path);
}

/** Returns the lowercase extension for downstream code that wants the
 *  raw extension rather than the human-readable label. */
export function extensionOf(path: string): string {
  return extOf(path);
}

/** Read a file's full contents using the same fallback chain as EditorPane. */
async function fetchFileBody(path: string): Promise<string> {
  try {
    const text = await invoke<string>("read_file_text", { path });
    if (typeof text === "string") return text;
  } catch {
    /* fall through */
  }
  return await readTextFile(path);
}

/**
 * Build a fresh excerpt by reading `path` and slicing `[start_line, end_line]`.
 * Clamps the range to the file's actual line count so a stale range from an
 * older edit doesn't blow up the splice math later.
 */
export async function buildExcerpt(
  path: string,
  start_line: number,
  end_line: number,
): Promise<MultibufferExcerpt> {
  const full = await fetchFileBody(path);
  const lines = full.split(/\r?\n/);
  const total = lines.length;
  // 1-based inclusive. Clamp + ensure start<=end.
  const start = Math.max(1, Math.min(start_line, total));
  const end = Math.max(start, Math.min(end_line, total));
  const body = lines.slice(start - 1, end).join("\n");
  return {
    id: newExcerptId(),
    path,
    language: languageLabel(path),
    start_line: start,
    end_line: end,
    body,
    dirty: false,
  };
}

/**
 * Shift the line ranges of sibling excerpts after an edit grows or shrinks a
 * region in the same file.
 *
 * When an excerpt is saved its replacement body may have a different line
 * count than the window it replaced (`delta` = added − removed). Every *other*
 * excerpt of the same file that starts strictly after the edited region is now
 * offset by `delta` and must be moved, otherwise the next save would splice at
 * stale line numbers and corrupt the file. We key off the store so the fix
 * holds even when excerpts are saved one after another (e.g. "Save All").
 *
 * `editedAfterLine` is the last line of the region that was replaced (1-based,
 * inclusive); only excerpts beginning after it are affected. The just-saved
 * excerpt (`savedId`) is skipped — its caller updates it directly.
 */
function reconcileSiblingExcerpts(
  path: string,
  savedId: string,
  editedAfterLine: number,
  delta: number,
): void {
  if (delta === 0) return;
  const store = useCortexStore.getState();
  let changed = false;
  const next = store.multibufferExcerpts.map((e) => {
    if (e.id === savedId || e.path !== path) return e;
    // Only excerpts that live entirely below the edited region move wholesale.
    if (e.start_line <= editedAfterLine) return e;
    changed = true;
    return {
      ...e,
      start_line: Math.max(1, e.start_line + delta),
      end_line: Math.max(1, e.end_line + delta),
    };
  });
  if (changed) store.setMultibufferExcerpts(next);
}

/**
 * Splice `newBody` into the file at `path`, replacing lines
 * `[start_line, end_line]`. Returns the new line range covered by the
 * replacement so the caller can keep the excerpt in sync (an excerpt may
 * grow or shrink relative to its original range).
 *
 * Read-modify-write: we re-fetch the file contents at save time so other
 * edits the user (or another tool) made outside the multibuffer aren't
 * clobbered.
 *
 * The excerpt's line range is re-derived from the live store entry (by id)
 * rather than trusting the passed-in copy: an earlier save of a sibling in the
 * same file may have shifted this excerpt's range since the caller snapshotted
 * it. After writing, sibling excerpts below the edit are shifted by the line
 * delta so subsequent saves splice at the correct offsets.
 */
export async function saveExcerpt(
  excerpt: MultibufferExcerpt,
): Promise<{ start_line: number; end_line: number; body: string }> {
  // Prefer the live range from the store (siblings may have shifted us).
  const live = useCortexStore
    .getState()
    .multibufferExcerpts.find((e) => e.id === excerpt.id);
  const start_line = live?.start_line ?? excerpt.start_line;
  const end_line = live?.end_line ?? excerpt.end_line;

  const full = await fetchFileBody(excerpt.path);
  const lines = full.split(/\r?\n/);
  const total = lines.length;
  const start = Math.max(1, Math.min(start_line, total + 1));
  const end = Math.max(start - 1, Math.min(end_line, total));
  const before = lines.slice(0, start - 1);
  const after = lines.slice(end);
  // An empty body means "delete this range": `"".split(...)` yields `[""]`,
  // which would splice in a stray blank line, so treat it as zero lines.
  const replacement = excerpt.body === "" ? [] : excerpt.body.split(/\r?\n/);
  const merged = [...before, ...replacement, ...after].join("\n");
  await saveFileText(excerpt.path, merged);

  const removedLines = end - (start - 1);
  const delta = replacement.length - removedLines;
  // Move any sibling excerpts of this file that sit below the edited window.
  reconcileSiblingExcerpts(excerpt.path, excerpt.id, end, delta);

  return {
    start_line: start,
    end_line: start + replacement.length - 1,
    body: excerpt.body,
  };
}

/**
 * Replace every excerpt currently in the multibuffer. Convenience wrapper
 * around the store mutator — callers prefer this over reaching into the
 * store directly so the API surface stays small.
 */
export function replaceMultibufferExcerpts(items: MultibufferExcerpt[]): void {
  useCortexStore.getState().setMultibufferExcerpts(items);
}

/**
 * Public helper for search / refactor agents: append (or replace) an
 * excerpt at the given path + range. Switches the activity panel to the
 * multibuffer tab so the user sees it land.
 */
export async function addExcerpt(
  path: string,
  start_line: number,
  end_line: number,
): Promise<MultibufferExcerpt> {
  const excerpt = await buildExcerpt(path, start_line, end_line);
  const store = useCortexStore.getState();
  store.setMultibufferExcerpts([...store.multibufferExcerpts, excerpt]);
  if (store.activityTab !== "multibuffer") {
    store.setActivityTab("multibuffer");
  }
  return excerpt;
}

/** Dispatch the `cortex:multibuffer-open` window event so non-React
 *  callers can hand a fresh excerpt list to the panel in one shot. */
export function dispatchMultibufferOpen(excerpts: MultibufferExcerpt[]): void {
  try {
    window.dispatchEvent(
      new CustomEvent<MultibufferOpenDetail>(MULTIBUFFER_OPEN_EVENT, {
        detail: { excerpts },
      }),
    );
  } catch {
    /* not in a browser env */
  }
}
