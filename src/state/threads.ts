/**
 * Thread primitives — kept in a sibling module so `store.ts` stays under the
 * 500 LOC cap. The store imports these helpers and the `Thread` type and
 * wires them up to its `set` calls.
 *
 * A `Thread` is a parallel chat lane within Cortex — Zed `cmd-alt-J` style.
 * Each thread owns its own `sessionId`, message array, in-flight run ids,
 * and routing reason. The store keeps one `activeThreadId` and mirrors the
 * active thread's per-message state onto the top-level fields for backwards
 * compatibility with components that haven't been migrated to the new model.
 */

import type { Message } from "./store";

export interface Thread {
  id: string;
  sessionId: string;
  label: string;
  /** User-chosen title from an inline rename. Wins over the derived
   *  first-message title when set; `null` means "derive as usual". */
  customTitle?: string | null;
  lastTs: number;
  messages: Message[];
  runningRunIds: string[];
  lastRoutingReason: string | null;
}

export const newSessionId = (): string => `session-${crypto.randomUUID()}`;
export const newThreadId = (): string => `thread-${crypto.randomUUID()}`;

export function makeThread(opts: {
  id?: string;
  sessionId?: string;
  label?: string;
  customTitle?: string | null;
  messages?: Message[];
  runningRunIds?: string[];
  lastRoutingReason?: string | null;
  lastTs?: number;
} = {}): Thread {
  return {
    id: opts.id ?? newThreadId(),
    sessionId: opts.sessionId ?? newSessionId(),
    label: opts.label ?? "thread 1",
    customTitle: opts.customTitle ?? null,
    lastTs: opts.lastTs ?? Date.now(),
    messages: opts.messages ?? [],
    runningRunIds: opts.runningRunIds ?? [],
    lastRoutingReason: opts.lastRoutingReason ?? null,
  };
}

/**
 * Shape of the slice of store state that `patchActiveThread` needs to read
 * and patch. Declared structurally so the store can pass `s` directly without
 * a circular type import.
 */
export interface ThreadHolder {
  threads: Thread[];
  activeThreadId: string;
}

/**
 * Apply `mutator` to the active thread and return a partial-state update
 * that mirrors the per-thread fields onto the top-level legacy mirrors
 * (`sessionId`, `messages`, `runningRunIds`, `lastRoutingReason`). The store
 * spreads this into its `set()` call.
 *
 * Returns `{}` if the active thread is somehow missing — callers should never
 * be in that state, but we no-op rather than crash.
 */
export function patchActiveThread<S extends ThreadHolder>(
  state: S,
  mutator: (t: Thread) => Thread,
): {
  threads?: Thread[];
  sessionId?: string;
  messages?: Message[];
  runningRunIds?: string[];
  lastRoutingReason?: string | null;
} {
  const idx = state.threads.findIndex((t) => t.id === state.activeThreadId);
  if (idx < 0) return {};
  const next = mutator(state.threads[idx]);
  const threads = state.threads.slice();
  threads[idx] = next;
  return {
    threads,
    sessionId: next.sessionId,
    messages: next.messages,
    runningRunIds: next.runningRunIds,
    lastRoutingReason: next.lastRoutingReason,
  };
}
