/**
 * Pinned notes (Open WebUI-style attachment rail).
 *
 * A "pinned note" is a short markdown blob — either pasted by hand or sourced
 * from an existing memory file — that gets prepended verbatim to every
 * outgoing chat message. The point is to bypass RAG: when the user has
 * something they ALWAYS want the model to see (style guide, current spec
 * fragment, etc.), retrieval is the wrong tool.
 *
 * Storage: a single JSON array at `~/.cortex/pinned-notes.json` mediated via
 * the existing `read_config_file` / `write_config_file` Tauri commands so we
 * don't need a dedicated backend module for v1.
 *
 * Caps:
 * - per-note content: 16 KiB
 * - per-note label: 80 chars
 * - max notes: 20 (the rail gets visually noisy past that)
 */

import { readConfigFile, writeConfigFile } from "@/lib/config-files";

const TARGET = { scope: "home" as const, rel_path: "pinned-notes.json" };

/** Hard caps on a single pinned-note's payload. Both enforced before we
 *  write to disk so we don't get into a state where the file is over the
 *  16 KiB-per-message budget. */
export const CONTENT_CAP_BYTES = 16 * 1024;
export const LABEL_CAP_CHARS = 80;
export const MAX_NOTES = 20;

export interface PinnedNote {
  id: string;
  label: string;
  content: string;
  /** Original file path when the note was sourced from a memory entry; null
   *  for inline-pasted notes. Surfaced in the preview popover. */
  source_path: string | null;
  pinned_at: number;
}

/** Fired (with no detail) whenever the on-disk list mutates. Components
 *  listen for this so the rail re-renders without a polling loop. */
export const PINNED_NOTES_EVENT = "cortex:pinned-notes-changed";

function emitChange() {
  try {
    window.dispatchEvent(new CustomEvent(PINNED_NOTES_EVENT));
  } catch {
    /* SSR / very early boot — no harm done. */
  }
}

function clampLabel(label: string): string {
  const trimmed = label.trim();
  if (trimmed.length <= LABEL_CAP_CHARS) return trimmed;
  return trimmed.slice(0, LABEL_CAP_CHARS - 1) + "…";
}

function clampContent(content: string): string {
  // We cap on byte length so multi-byte chars can't sneak past the limit.
  // Using TextEncoder once is cheaper than computing UTF-8 length manually.
  const encoder = new TextEncoder();
  const bytes = encoder.encode(content);
  if (bytes.length <= CONTENT_CAP_BYTES) return content;
  // Trim back to roughly the cap, then re-decode to drop any half-truncated
  // code point at the boundary.
  const decoder = new TextDecoder("utf-8", { fatal: false, ignoreBOM: true });
  return decoder.decode(bytes.slice(0, CONTENT_CAP_BYTES)) + "\n…(truncated)";
}

function makeId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `pin-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
  }
}

/** Parse + validate the JSON envelope. Invalid entries are dropped silently
 *  rather than failing the whole load — a corrupt single record shouldn't
 *  wipe the rail. */
function decode(body: string): PinnedNote[] {
  if (!body.trim()) return [];
  try {
    const parsed = JSON.parse(body);
    if (!Array.isArray(parsed)) return [];
    const out: PinnedNote[] = [];
    for (const raw of parsed) {
      if (!raw || typeof raw !== "object") continue;
      const id = typeof raw.id === "string" ? raw.id : null;
      const label = typeof raw.label === "string" ? raw.label : null;
      const content = typeof raw.content === "string" ? raw.content : null;
      if (!id || !label || content == null) continue;
      out.push({
        id,
        label: clampLabel(label),
        content: clampContent(content),
        source_path:
          typeof raw.source_path === "string" ? raw.source_path : null,
        pinned_at:
          typeof raw.pinned_at === "number" ? raw.pinned_at : Date.now(),
      });
    }
    return out;
  } catch {
    return [];
  }
}

/** Read the on-disk list. Returns [] when the file is missing or corrupt. */
export async function listPinnedNotes(): Promise<PinnedNote[]> {
  try {
    const res = await readConfigFile(TARGET);
    if (!res.exists) return [];
    return decode(res.body);
  } catch {
    return [];
  }
}

async function writeAll(notes: PinnedNote[]): Promise<void> {
  // Cap from the end so the most recently added notes are kept; slicing from
  // the front would silently drop a freshly appended Nth note.
  const trimmed =
    notes.length > MAX_NOTES ? notes.slice(notes.length - MAX_NOTES) : notes;
  await writeConfigFile(TARGET, JSON.stringify(trimmed, null, 2));
  emitChange();
}

/** Append a new note. No-op (returns the existing entry) when a note with
 *  the same source_path is already pinned, so `/pin <path>` is idempotent. */
export async function addPinnedNote(input: {
  label: string;
  content: string;
  source_path?: string | null;
}): Promise<PinnedNote> {
  const all = await listPinnedNotes();
  if (input.source_path) {
    const existing = all.find((n) => n.source_path === input.source_path);
    if (existing) return existing;
  }
  const note: PinnedNote = {
    id: makeId(),
    label: clampLabel(input.label) || "untitled",
    content: clampContent(input.content),
    source_path: input.source_path ?? null,
    pinned_at: Date.now(),
  };
  const next = [...all, note];
  await writeAll(next);
  return note;
}

export async function removePinnedNote(id: string): Promise<void> {
  const all = await listPinnedNotes();
  const next = all.filter((n) => n.id !== id);
  if (next.length === all.length) return;
  await writeAll(next);
}

/** Reorder by moving `id` to `targetIndex`. Used by drag-and-drop in the
 *  rail. Out-of-range indices clamp to the nearest valid slot. */
export async function reorderPinnedNotes(
  id: string,
  targetIndex: number,
): Promise<void> {
  const all = await listPinnedNotes();
  const fromIdx = all.findIndex((n) => n.id === id);
  if (fromIdx < 0) return;
  const note = all[fromIdx];
  const without = all.filter((_, i) => i !== fromIdx);
  const clamped = Math.max(0, Math.min(targetIndex, without.length));
  without.splice(clamped, 0, note);
  await writeAll(without);
}

/** Concatenate every pinned note into a single markdown block suitable for
 *  prepending to an outgoing chat message. Returns "" when nothing's pinned
 *  so the caller can skip the prefix entirely. */
export function formatForPrepend(notes: PinnedNote[]): string {
  if (notes.length === 0) return "";
  const sections = notes.map((n) => {
    const head = n.source_path
      ? `### 📌 ${n.label} (\`${n.source_path}\`)`
      : `### 📌 ${n.label}`;
    return `${head}\n\n${n.content}`;
  });
  return sections.join("\n\n") + "\n\n---\n\n";
}
