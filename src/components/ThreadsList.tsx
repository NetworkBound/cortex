/**
 * ThreadsList — sidebar picker for the parallel chat threads owned by the
 * current project (or the persisted global bucket when no project is active).
 *
 * Wiring:
 *   - On mount + whenever `activeProject` changes, calls `list_threads` and
 *     merges the result into the store via `setThreads`. With no project the
 *     root resolves to `GLOBAL_PROJECT_ROOT_KEY`, which the backend maps to
 *     `~/.cortex/threads/` — global threads persist like project ones, and a
 *     hint row tells the user where they live.
 *   - Every 5s, if the active thread has any messages, persists it via
 *     `save_thread`. This is intentionally a coarse debounce — chat streams
 *     emit token-by-token; we don't want to thrash the disk per token. To
 *     keep the 5s window from eating the tail of a conversation, the active
 *     thread is also flushed on thread switch and on `beforeunload`.
 *   - "+ New thread" mints a fresh `Thread` via the store's `newThread`
 *     action, then immediately persists so the empty thread shows up in
 *     `list_threads` results across remounts.
 *   - "switch" calls `switchThread` which already replaces the legacy
 *     top-level mirrors (`messages`, `sessionId`, etc.) with the target
 *     thread's in-memory copy.
 *   - "Rename" swaps the title for an inline input (Enter/blur saves, Esc
 *     cancels). The custom title is stored on the thread (`customTitle`) and
 *     persisted immediately; clearing it falls back to the derived title.
 *   - "delete" removes from disk first, then from the store. On error we
 *     swallow + log — the in-memory remove still runs so the UI matches the
 *     user's intent even if the file is locked.
 *
 * Keep this under 300 LOC; the related styles live in `global.css`.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { timeAgo } from "@/lib/time";
import { useCortexStore } from "@/state/store";
import {
  deleteThread as deleteThreadIpc,
  deriveThreadTitle,
  listThreads,
  resolveProjectRoot,
  saveThread,
} from "@/lib/threads";
import type { Thread } from "@/state/threads";

const AUTOSAVE_INTERVAL_MS = 5_000;

/** Best-effort flush of the active thread to disk (no-op when empty). */
function flushActiveThread(projectRoot: string) {
  const state = useCortexStore.getState();
  const active = state.threads.find((t) => t.id === state.activeThreadId);
  if (!active || active.messages.length === 0) return;
  void saveThread(projectRoot, active);
}

export function ThreadsList() {
  const threads = useCortexStore((s) => s.threads);
  const activeThreadId = useCortexStore((s) => s.activeThreadId);
  const messages = useCortexStore((s) => s.messages);
  const activeProject = useCortexStore((s) => s.activeProject);
  const setThreads = useCortexStore((s) => s.setThreads);
  const newThread = useCortexStore((s) => s.newThread);
  const switchThread = useCortexStore((s) => s.switchThread);
  const removeThread = useCortexStore((s) => s.removeThread);
  const renameThread = useCortexStore((s) => s.renameThread);
  const resetSession = useCortexStore((s) => s.resetSession);

  const projectRoot = useMemo(
    () => resolveProjectRoot(activeProject?.root ?? null),
    [activeProject?.root],
  );

  // Hydrate from disk whenever the project context shifts.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const disk = await listThreads(projectRoot);
      if (!cancelled && disk.length > 0) setThreads(disk);
    })();
    return () => {
      cancelled = true;
    };
  }, [projectRoot, setThreads]);

  // Coarse autosave loop — runs at most once per AUTOSAVE_INTERVAL_MS even
  // when the message array changes on every streamed token. The interval is
  // re-armed when the active thread switches so we don't accidentally save
  // the wrong thread under the new id.
  const lastSavedRef = useRef<{ id: string | null; ts: number }>({ id: null, ts: 0 });
  useEffect(() => {
    const tick = () => {
      const state = useCortexStore.getState();
      const active = state.threads.find((t) => t.id === state.activeThreadId);
      if (!active) return;
      if (active.messages.length === 0) return;
      const sig = lastSavedRef.current;
      if (sig.id === active.id && sig.ts === active.lastTs) return;
      lastSavedRef.current = { id: active.id, ts: active.lastTs };
      void saveThread(projectRoot, active);
    };
    const id = setInterval(tick, AUTOSAVE_INTERVAL_MS);
    return () => clearInterval(id);
    // `messages` is in the deps so the effect re-runs when the active thread
    // streams, but the interval itself remains the rate-limiter.
  }, [projectRoot, activeThreadId, messages]);

  // Flush the active thread when the window goes away so the autosave
  // interval can't drop the final seconds of a conversation. Best-effort:
  // the IPC call may not complete during teardown, but it usually does.
  useEffect(() => {
    const flush = () => flushActiveThread(projectRoot);
    window.addEventListener("beforeunload", flush);
    return () => window.removeEventListener("beforeunload", flush);
  }, [projectRoot]);

  const handleNew = useCallback(() => {
    // Reset the session machinery FIRST so token-stream state doesn't bleed
    // into the new thread, then mint a fresh thread on top.
    resetSession();
    const id = newThread();
    // Persist the empty shell so it survives a full reload.
    const t = useCortexStore.getState().threads.find((x) => x.id === id);
    if (t) void saveThread(projectRoot, t);
  }, [newThread, resetSession, projectRoot]);

  const handleSwitch = useCallback(
    (id: string) => {
      // Persist whatever the outgoing thread has before its mirrors are
      // replaced — switching is the other place the 5s window could lose a
      // message tail.
      flushActiveThread(projectRoot);
      switchThread(id);
    },
    [projectRoot, switchThread],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      const t = useCortexStore.getState().threads.find((x) => x.id === id);
      const ok = await confirmDialog({
        title: "Delete thread?",
        message: `"${t?.label ?? "Untitled thread"}" and its messages will be deleted. This cannot be undone.`,
        confirmLabel: "Delete",
        danger: true,
      });
      if (!ok) return;
      await deleteThreadIpc(projectRoot, id);
      removeThread(id);
    },
    [projectRoot, removeThread],
  );

  const handleRename = useCallback(
    (id: string, title: string) => {
      renameThread(id, title);
      const t = useCortexStore.getState().threads.find((x) => x.id === id);
      if (t) void saveThread(projectRoot, t);
    },
    [projectRoot, renameThread],
  );

  const sorted = useMemo(
    () => threads.slice().sort((a, b) => b.lastTs - a.lastTs),
    [threads],
  );

  return (
    <div className="threads-list">
      <div className="threads-list-head">
        <button className="threads-new-btn" onClick={handleNew} type="button">
          + New thread
        </button>
        <span className="threads-scope">
          {activeProject ? activeProject.name : "Global"}
        </span>
      </div>
      {!activeProject && (
        <div className="threads-hint">
          No project open — these threads are saved to your global space and
          stay available across restarts.
        </div>
      )}
      <div className="threads-list-body">
        {sorted.length === 0 && (
          <div className="muted threads-empty">
            No threads yet. Start one with “+ New thread” to keep parallel
            conversations side by side.
          </div>
        )}
        {sorted.map((t) => (
          <ThreadRow
            key={t.id}
            thread={t}
            active={t.id === activeThreadId}
            onSwitch={() => handleSwitch(t.id)}
            onDelete={() => void handleDelete(t.id)}
            onRename={(title) => handleRename(t.id, title)}
          />
        ))}
      </div>
    </div>
  );
}

function ThreadRow({
  thread,
  active,
  onSwitch,
  onDelete,
  onRename,
}: {
  thread: Thread;
  active: boolean;
  onSwitch: () => void;
  onDelete: () => void;
  onRename: (title: string) => void;
}) {
  const title = deriveThreadTitle(thread);
  const count = thread.messages.length;
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  const beginRename = () => {
    setDraft(thread.customTitle ?? title);
    setEditing(true);
  };
  const commitRename = () => {
    if (!editing) return;
    setEditing(false);
    // An empty draft clears the override → title derives from the first
    // message again. No-op when nothing changed.
    const next = draft.trim();
    if (next !== (thread.customTitle ?? "").trim()) onRename(next);
  };

  return (
    <div className={`thread-row${active ? " thread-row--active" : ""}`}>
      {editing ? (
        <div className="thread-row-main thread-row-main--editing">
          <input
            className="thread-row-rename"
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commitRename();
              } else if (e.key === "Escape") {
                e.preventDefault();
                setEditing(false);
              }
            }}
            onBlur={commitRename}
            placeholder="Thread title…"
            aria-label="thread title"
          />
        </div>
      ) : (
        <button
          className="thread-row-main"
          onClick={onSwitch}
          onDoubleClick={beginRename}
          type="button"
          title={title}
        >
          <div className="thread-row-title">{title}</div>
          <div className="thread-row-meta">
            <span className="muted">{timeAgo(thread.lastTs)}</span>
            <span className="thread-row-badge">{count}</span>
          </div>
        </button>
      )}
      <div className="thread-row-actions">
        {!active && !editing && (
          <button
            className="link-btn"
            onClick={onSwitch}
            type="button"
            aria-label="switch to thread"
          >
            Switch
          </button>
        )}
        {!editing && (
          <button
            className="link-btn"
            onClick={(e) => {
              e.stopPropagation();
              beginRename();
            }}
            type="button"
            aria-label="rename thread"
          >
            Rename
          </button>
        )}
        <button
          className="link-btn link-btn--danger"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          type="button"
          aria-label="delete thread"
        >
          Delete
        </button>
      </div>
    </div>
  );
}

