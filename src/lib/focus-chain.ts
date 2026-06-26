// Focus-chain — the agent-managed live to-do list.
//
// The agent emits `update_focus_chain(items)` mid-task; whoever owns the
// agent_event stream wires that into the store mutators below. The list is
// shown in the ActivityPanel (FocusChain.tsx) and persisted per `session_id`
// at `~/.cortex/focus-chains/<id>.json` so resuming a chat restores the
// chain.
//
// We piggy-back on the Zustand store in `state/store.ts` rather than holding
// our own state here — that way thread-switch / resumeSession can clear or
// rehydrate the chain alongside the rest of the session.
//
// Persistence is best-effort: every mutator triggers a fire-and-forget save
// to the backend. Loads degrade to `[]` on failure (a missing file is the
// normal "no chain yet" state for a fresh session).

import { invoke } from "@tauri-apps/api/core";
import { useCortexStore } from "@/state/store";

export interface FocusChainTask {
  /** Stable id within the chain. Generated client-side if the agent omits it. */
  id: string;
  title: string;
  done: boolean;
}

/** Shape persisted to disk — `id` is regenerated on load so we don't store it. */
interface PersistedTask {
  title: string;
  done: boolean;
}

function nextId(): string {
  return `fc-${crypto.randomUUID()}`;
}

/** Hydrate a persisted list back into the store form. */
function fromPersisted(items: PersistedTask[]): FocusChainTask[] {
  return items.map((t) => ({ id: nextId(), title: t.title, done: !!t.done }));
}

function toPersisted(items: FocusChainTask[]): PersistedTask[] {
  return items.map(({ title, done }) => ({ title, done }));
}

/** Fire-and-forget save — never throws. */
function persist(sessionId: string, items: FocusChainTask[]): void {
  void invoke("save_focus_chain", { sessionId, items: toPersisted(items) }).catch(
    (err) => console.warn("save_focus_chain failed", err),
  );
}

export async function loadFocusChain(sessionId: string): Promise<FocusChainTask[]> {
  try {
    const raw = await invoke<PersistedTask[]>("load_focus_chain", { sessionId });
    return fromPersisted(raw ?? []);
  } catch (err) {
    console.warn("load_focus_chain failed", err);
    return [];
  }
}

export async function clearFocusChainOnDisk(sessionId: string): Promise<void> {
  try {
    await invoke("clear_focus_chain", { sessionId });
  } catch (err) {
    console.warn("clear_focus_chain failed", err);
  }
}

// ── Store-backed mutators ───────────────────────────────────────────────
//
// These are the public surface the agent_event handler (lives elsewhere) is
// expected to call when it sees `update_focus_chain` tool calls. They update
// the store synchronously and kick off a background persist so the next
// resume sees the latest state.

/** Append a single task. No-op on empty/whitespace title. */
export function addTask(title: string): void {
  const trimmed = title.trim();
  if (!trimmed) return;
  const s = useCortexStore.getState();
  const next = [...s.focusChain, { id: nextId(), title: trimmed, done: false }];
  s.setFocusChain(next);
  persist(s.sessionId, next);
}

/** Toggle / set the `done` flag for a task by id. */
export function tickTask(id: string, done = true): void {
  const s = useCortexStore.getState();
  const next = s.focusChain.map((t) => (t.id === id ? { ...t, done } : t));
  s.setFocusChain(next);
  persist(s.sessionId, next);
}

/** Wipe the entire chain (UI + disk). */
export function clearChain(): void {
  const s = useCortexStore.getState();
  s.setFocusChain([]);
  void clearFocusChainOnDisk(s.sessionId);
}

/**
 * Replace the whole chain at once — used by the agent when it emits a fresh
 * `update_focus_chain(items)` call rather than incremental adds. Items
 * without a stable id get a fresh one.
 */
export function replaceChain(
  items: Array<{ id?: string; title: string; done?: boolean }>,
): void {
  const next: FocusChainTask[] = items
    .filter((t) => t.title && t.title.trim())
    .map((t) => ({
      id: t.id ?? nextId(),
      title: t.title.trim(),
      done: !!t.done,
    }));
  const s = useCortexStore.getState();
  s.setFocusChain(next);
  persist(s.sessionId, next);
}
