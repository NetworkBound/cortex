// Bookmarks / favorites store, backed by `~/.cortex/bookmarks.json`.
//
// Wraps the five Tauri commands (`list_bookmarks`, `add_bookmark`,
// `update_bookmark`, `delete_bookmark`, `touch_bookmark`) and provides a
// tiny dispatch helper for opening a bookmark in the appropriate surface
// (editor pane for memory/file, trace viewer for trace ids, session resume
// for chat sessions, external shell for URLs, info toast for notes).
//
// Backend storage shape matches `commands/bookmarks.rs`. All commands are
// best-effort: on failure they degrade to "empty list" / "no-op" rather
// than throwing, so the panel stays usable even when the JSON file is
// missing or corrupt.

import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { openInEditor } from "@/lib/editor";
import { loadSessionMessages } from "@/lib/sessions";
import { pushToast } from "@/lib/toast";
import { useCortexStore, type Message } from "@/state/store";

export type BookmarkKind =
  | "memory"
  | "file"
  | "trace"
  | "session"
  | "url"
  | "note";

export const BOOKMARK_KINDS: BookmarkKind[] = [
  "memory",
  "file",
  "trace",
  "session",
  "url",
  "note",
];

/** Label shown in the panel + kind dropdown. Mirrors the backend allow-list. */
export const BOOKMARK_KIND_LABELS: Record<BookmarkKind, string> = {
  memory: "Memory entry",
  file: "File",
  trace: "Trace",
  session: "Chat session",
  url: "URL",
  note: "Note",
};

export interface Bookmark {
  id: string;
  kind: BookmarkKind;
  label: string;
  target: string;
  tags: string[];
  note: string | null;
  created_unix_ms: number;
  last_opened_unix_ms: number | null;
}

/** Custom event the trace viewer (best-effort) listens for. */
export const BOOKMARK_TRACE_OPEN_EVENT = "cortex:trace-open";

/** Custom event the activity panel can listen for to refresh after writes. */
export const BOOKMARK_CHANGED_EVENT = "cortex:bookmarks-changed";

function fireChanged(): void {
  try {
    window.dispatchEvent(new CustomEvent(BOOKMARK_CHANGED_EVENT));
  } catch {
    /* not in a browser-like env */
  }
}

export async function listBookmarks(
  filterKind?: BookmarkKind,
  filterTag?: string,
): Promise<Bookmark[]> {
  // Let the caller distinguish "backend unreachable" from "no bookmarks" — the
  // old swallow-to-[] made a down backend look like an empty list (the panel
  // then showed its "No bookmarks yet" empty state instead of an error).
  return invoke<Bookmark[]>("list_bookmarks", {
    filterKind: filterKind ?? null,
    filterTag: filterTag ?? null,
  });
}

/**
 * Add (or upsert) a bookmark. The backend assigns a ULID when `id` is empty
 * and merges duplicates on `(kind, target)` so re-bookmarking the same file
 * doesn't litter the list with copies.
 */
export async function addBookmark(
  input: Omit<Bookmark, "id" | "created_unix_ms" | "last_opened_unix_ms"> & {
    id?: string;
  },
): Promise<Bookmark | null> {
  const payload: Bookmark = {
    id: input.id ?? "",
    kind: input.kind,
    label: input.label,
    target: input.target,
    tags: input.tags ?? [],
    note: input.note ?? null,
    created_unix_ms: 0,
    last_opened_unix_ms: null,
  };
  try {
    const saved = await invoke<Bookmark>("add_bookmark", { bookmark: payload });
    fireChanged();
    return saved;
  } catch (err) {
    console.warn("addBookmark failed", err);
    return null;
  }
}

export async function updateBookmark(b: Bookmark): Promise<boolean> {
  try {
    await invoke("update_bookmark", { bookmark: b });
    fireChanged();
    return true;
  } catch (err) {
    console.warn("updateBookmark failed", err);
    return false;
  }
}

export async function deleteBookmark(id: string): Promise<boolean> {
  try {
    await invoke("delete_bookmark", { id });
    fireChanged();
    return true;
  } catch (err) {
    console.warn("deleteBookmark failed", err);
    return false;
  }
}

export async function touchBookmark(id: string): Promise<void> {
  try {
    await invoke("touch_bookmark", { id });
  } catch (err) {
    console.warn("touchBookmark failed", err);
  }
}

/**
 * Open a bookmark in the appropriate surface. Each kind routes to a
 * different handler:
 *   memory / file → editor pane via `cortex:editor-open`
 *   trace         → best-effort `cortex:trace-open` window event
 *   session       → resume in chat via store.resumeSession
 *   url           → `@tauri-apps/plugin-shell`'s `open`
 *   note          → info toast (free-form text, no destination)
 *
 * Always bumps `last_opened_unix_ms` so the panel sort reflects recency.
 */
export async function openBookmark(b: Bookmark): Promise<void> {
  void touchBookmark(b.id);
  switch (b.kind) {
    case "memory":
    case "file": {
      openInEditor(b.target);
      return;
    }
    case "trace": {
      try {
        window.dispatchEvent(
          new CustomEvent(BOOKMARK_TRACE_OPEN_EVENT, {
            detail: { trace_id: b.target },
          }),
        );
        useCortexStore.getState().setActivityTab("observability");
      } catch {
        pushToast({
          title: "Couldn't open trace",
          body: b.target,
          kind: "warning",
        });
      }
      return;
    }
    case "session": {
      try {
        const stored = await loadSessionMessages(b.target);
        const msgs: Message[] = stored.map((m) => ({
          id: m.id,
          role: (m.role as Message["role"]) || "assistant",
          agent: m.agent_id ?? undefined,
          content: m.content,
          reasoning: m.reasoning ?? undefined,
          pending: false,
          tools: [],
          runId: m.run_id,
        }));
        useCortexStore.getState().resumeSession(b.target, msgs);
      } catch (err) {
        pushToast({
          title: "Resume failed",
          body: humanizeError(err),
          kind: "error",
        });
      }
      return;
    }
    case "url": {
      try {
        await shellOpen(b.target);
      } catch (err) {
        pushToast({
          title: "Couldn't open URL",
          body: humanizeError(err),
          kind: "error",
        });
      }
      return;
    }
    case "note": {
      pushToast({
        title: b.label,
        body: b.note ?? b.target,
        kind: "info",
      });
      return;
    }
  }
}

/** Parse a comma-separated tag input into a clean array. */
export function parseTags(raw: string): string[] {
  return raw
    .split(",")
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

export { timeAgo } from "@/lib/time";
