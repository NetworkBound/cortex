/**
 * "Needs attention" — one derived view over the thread store answering the
 * three questions a cockpit for many agents has to answer at a glance:
 *
 *   1. Which runs are PAUSED waiting for me?  (`Message.approval` across
 *      every thread, not just the active one)
 *   2. What is RUNNING right now?              (`Thread.runningRunIds`)
 *   3. What FAILED recently?                   (error turns + failed jobs)
 *
 * Everything here is pure selectors over `useCortexStore().threads` plus a
 * small store watcher that turns a *new* pending approval into a toast, an
 * optional sound, an OS notification (only when the window is hidden or
 * unfocused) and an inbox row in the notification center — and removes that
 * row again the moment the approval resolves, whichever path resolved it
 * (ApprovalPrompt click, `approval_resolved` event, thread deleted…).
 *
 * Consumers:
 *   - `ActivityBar`      → `usePendingApprovalCount()` badge on Threads
 *   - `ThreadsList`      → `useAttention().approvals` per-thread dot
 *   - `TodayDashboard`   → `useAttention()` "Needs attention" card
 *   - palette / keymap   → `focusOldestApproval()` or dispatch
 *                          `new CustomEvent("cortex:focus-approval")`
 *   - notification inbox → `focusApproval({ threadId, messageId })`
 *
 * The pure helpers (`collectApprovals`, `collectRunning`, `collectFailures`,
 * `oldestApproval`, `approvalCountByThread`) take plain data so they can be
 * unit-tested without React or Tauri.
 */

import { useMemo } from "react";
import { useCortexStore, type Message } from "@/state/store";
import type { Thread } from "@/state/threads";
import { stopRun } from "@/lib/cortex-bridge";
import {
  deriveThreadTitle,
  resolveProjectRoot,
  saveThread,
} from "@/lib/threads";
import {
  clearApprovalEvent,
  recordApprovalEvent,
  useNotifications,
  type Notification,
} from "@/lib/notification-center";
import { desktopNotify } from "@/lib/notify";
import { playSound } from "@/lib/sounds";
import { pushToast } from "@/lib/toast";
import { truncate } from "@/lib/format";
import { humanizeError } from "@/lib/errors";

// ── Types ──────────────────────────────────────────────────────────────────

/** One tool call paused for the user's decision, located in the thread tree. */
export interface ApprovalRef {
  threadId: string;
  threadTitle: string;
  messageId: string;
  /** `PendingApproval.id` — stable per request; the inbox row key. */
  approvalId: string;
  runId: string;
  agent: string;
  tool: string | null;
  preview: string | null;
  /** When the request arrived (`PendingApproval.receivedAt`). */
  ts: number;
}

/** A thread with at least one in-flight run. */
export interface RunningThread {
  threadId: string;
  threadTitle: string;
  runIds: string[];
  /** Agent id of the streaming assistant turn, when known. */
  agent: string | null;
  /** Best available "since" — the thread's last activity stamp. */
  since: number;
}

/** Something that went wrong recently: an error turn or a failed job. */
export interface RecentFailure {
  id: string;
  ts: number;
  /** Set for chat errors so the row can jump to the thread. */
  threadId: string | null;
  messageId: string | null;
  label: string;
  detail: string | null;
}

export interface AttentionSnapshot {
  /** Oldest first — the one the user should answer next is `approvals[0]`. */
  approvals: ApprovalRef[];
  approvalCount: number;
  running: RunningThread[];
  runningCount: number;
  failedRecent: RecentFailure[];
  /** approvals + failures — what the rail should shout about. */
  total: number;
}

/** Window event other surfaces (palette, keymap) dispatch to jump to a pending
 *  approval. `detail` may carry `{ threadId, messageId }`; without it the
 *  OLDEST pending approval is focused. */
export const FOCUS_APPROVAL_EVENT = "cortex:focus-approval";

/** Chat error turns older than this are no longer "recent". */
const CHAT_FAILURE_WINDOW_MS = 60 * 60 * 1000;
/** Failed background jobs (routines, evals, pulls) stay listed this long. */
const JOB_FAILURE_WINDOW_MS = 24 * 60 * 60 * 1000;
/** Approvals first seen more than this long after they arrived are treated as
 *  rehydrated history: listed + badged, but not toasted/notified again. */
const FRESH_APPROVAL_MS = 60 * 1000;
const MAX_FAILURES = 6;

// ── Pure selectors ─────────────────────────────────────────────────────────

/** Every pending approval across all threads, oldest first. */
export function collectApprovals(threads: readonly Thread[]): ApprovalRef[] {
  const out: ApprovalRef[] = [];
  for (const t of threads) {
    let title: string | null = null;
    for (const m of t.messages) {
      const a = m.approval;
      if (!a) continue;
      title ??= deriveThreadTitle(t);
      out.push({
        threadId: t.id,
        threadTitle: title,
        messageId: m.id,
        approvalId: a.id,
        runId: a.runId,
        agent: a.agent,
        tool: a.tool,
        preview: a.preview,
        ts: a.receivedAt,
      });
    }
  }
  out.sort((x, y) => x.ts - y.ts);
  return out;
}

/** Cheap count used by the rail badge selector (runs on every store change,
 *  so it avoids allocating). */
export function countApprovals(threads: readonly Thread[]): number {
  let n = 0;
  for (const t of threads) for (const m of t.messages) if (m.approval) n++;
  return n;
}

/** The approval the user should answer next (oldest), or null. */
export function oldestApproval(
  list: readonly ApprovalRef[],
): ApprovalRef | null {
  if (list.length === 0) return null;
  let best = list[0];
  for (const a of list) if (a.ts < best.ts) best = a;
  return best;
}

/** threadId → number of pending approvals in that thread. */
export function approvalCountByThread(
  list: readonly ApprovalRef[],
): Map<string, number> {
  const m = new Map<string, number>();
  for (const a of list) m.set(a.threadId, (m.get(a.threadId) ?? 0) + 1);
  return m;
}

function lastAssistant(messages: readonly Message[]): Message | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "assistant") return messages[i];
  }
  return null;
}

/** Threads with in-flight runs, most recently active first. */
export function collectRunning(threads: readonly Thread[]): RunningThread[] {
  const out: RunningThread[] = [];
  for (const t of threads) {
    if (t.runningRunIds.length === 0) continue;
    out.push({
      threadId: t.id,
      threadTitle: deriveThreadTitle(t),
      runIds: t.runningRunIds.slice(),
      agent: lastAssistant(t.messages)?.agent ?? null,
      since: t.lastTs,
    });
  }
  out.sort((x, y) => y.since - x.since);
  return out;
}

/**
 * Recent failures: error turns from threads active inside the window (chat
 * messages carry no timestamp, so the thread's `lastTs` stands in) plus
 * failed jobs from the notification inbox. Newest first, capped.
 */
export function collectFailures(
  threads: readonly Thread[],
  notifications: readonly Notification[],
  now: number,
): RecentFailure[] {
  const out: RecentFailure[] = [];
  for (const t of threads) {
    if (now - t.lastTs > CHAT_FAILURE_WINDOW_MS) continue;
    // Only the tail of the transcript is "recent" in any useful sense.
    const tail = t.messages.slice(-8);
    for (const m of tail) {
      if (m.role !== "error") continue;
      out.push({
        id: `chat:${t.id}:${m.id}`,
        ts: t.lastTs,
        threadId: t.id,
        messageId: m.id,
        label: deriveThreadTitle(t),
        detail: truncate(m.content.replace(/^error:\s*/i, ""), 120),
      });
    }
  }
  for (const n of notifications) {
    if (n.source !== "job" || n.severity !== "error") continue;
    if (now - n.ts > JOB_FAILURE_WINDOW_MS) continue;
    out.push({
      id: n.id,
      ts: n.ts,
      threadId: null,
      messageId: null,
      label: n.message,
      detail: n.detail ? truncate(n.detail, 120) : null,
    });
  }
  out.sort((x, y) => y.ts - x.ts);
  return out.slice(0, MAX_FAILURES);
}

/** Assemble the full snapshot from plain inputs. */
export function buildAttention(
  threads: readonly Thread[],
  notifications: readonly Notification[],
  now: number,
): AttentionSnapshot {
  const approvals = collectApprovals(threads);
  const running = collectRunning(threads);
  const failedRecent = collectFailures(threads, notifications, now);
  return {
    approvals,
    approvalCount: approvals.length,
    running,
    runningCount: running.length,
    failedRecent,
    total: approvals.length + failedRecent.length,
  };
}

// ── React hooks ────────────────────────────────────────────────────────────

/** Live snapshot. Re-derives whenever the thread list or the inbox changes. */
export function useAttention(): AttentionSnapshot {
  const threads = useCortexStore((s) => s.threads);
  const notifications = useNotifications();
  return useMemo(
    () => buildAttention(threads, notifications, Date.now()),
    [threads, notifications],
  );
}

/** Primitive selector for badges — the component only re-renders when the
 *  number itself changes, not on every streamed token. */
export function usePendingApprovalCount(): number {
  return useCortexStore((s) => countApprovals(s.threads));
}

// ── Imperative navigation ──────────────────────────────────────────────────

/** Switch to the approval's thread and ask the transcript to scroll to and
 *  highlight its message. Resolves `false` when the thread no longer exists. */
export async function focusApproval(target: {
  threadId: string;
  messageId: string;
}): Promise<boolean> {
  const st = useCortexStore.getState();
  const thread = st.threads.find((t) => t.id === target.threadId);
  if (!thread) return false;
  if (st.activeThreadId !== thread.id) {
    // Same flush ThreadsList does before a switch, so the outgoing thread's
    // last few seconds aren't lost to the 5s autosave window.
    const outgoing = st.getActiveThread();
    if (outgoing && outgoing.messages.length > 0) {
      void saveThread(
        resolveProjectRoot(st.activeProject?.root ?? null),
        outgoing,
      );
    }
    st.switchThread(thread.id);
  }
  useCortexStore.getState().setAttentionFocusMessageId(target.messageId);
  return true;
}

/** Jump to the oldest pending approval. Returns false (with a small toast)
 *  when nothing is waiting — so a keybinding gives feedback either way. */
export function focusOldestApproval(): boolean {
  const next = oldestApproval(
    collectApprovals(useCortexStore.getState().threads),
  );
  if (!next) {
    pushToast({ title: "No pending approvals", kind: "info", ttlMs: 2000 });
    return false;
  }
  void focusApproval(next);
  return true;
}

/** Stop every in-flight run in `threadId`. Each id is untracked as soon as
 *  the backend accepts the stop, so a background thread (whose `done` event
 *  nobody is subscribed to) doesn't keep a phantom "running" badge. */
export async function stopThreadRuns(threadId: string): Promise<void> {
  const thread = useCortexStore
    .getState()
    .threads.find((t) => t.id === threadId);
  if (!thread) return;
  for (const runId of thread.runningRunIds) {
    try {
      await stopRun(runId);
      useCortexStore.getState().untrackRunIdInThread(threadId, runId);
    } catch (e) {
      pushToast({
        title: "Couldn't stop run",
        body: humanizeError(e),
        kind: "error",
      });
    }
  }
}

// ── Store watcher: new approval → toast / sound / OS notification / inbox ──

/** Approval ids already announced + recorded, so a store update that merely
 *  re-creates message objects (every streamed token does) can't re-fire. */
const known = new Map<string, ApprovalRef>();
let watcherInstalled = false;

function windowIsInBackground(): boolean {
  try {
    return document.hidden || !document.hasFocus();
  } catch {
    return false;
  }
}

function announce(a: ApprovalRef, activeThreadId: string): void {
  const tool = a.tool ?? "tool call";
  const title = `Approval needed · ${tool}`;
  const body = a.preview
    ? `${a.threadTitle}: ${truncate(a.preview, 140)}`
    : `${a.agent} is waiting in "${a.threadTitle}".`;
  const background = windowIsInBackground();
  // The inline prompt is already on screen when the approval belongs to the
  // active thread and the window is in front — a toast on top would just be
  // noise. Everywhere else (other thread, or window hidden) show it with a
  // one-click "Open" that jumps straight to the prompt.
  if (a.threadId !== activeThreadId || background) {
    pushToast({
      title,
      body,
      kind: "warning",
      ttlMs: 10_000,
      action: { label: "Open", onClick: () => void focusApproval(a) },
    });
  }
  // Gated on the user's sound preference inside playSound.
  playSound("approve");
  if (background) {
    // OS toast only when they've switched away; failures (no notification
    // daemon on headless Linux, etc.) are fine — the in-app toast is enough.
    void desktopNotify(title, body).catch(() => {});
  }
}

function diffApprovals(threads: readonly Thread[], activeThreadId: string) {
  const current = collectApprovals(threads);
  const currentIds = new Set<string>();
  const now = Date.now();
  for (const a of current) {
    currentIds.add(a.approvalId);
    if (known.has(a.approvalId)) continue;
    known.set(a.approvalId, a);
    recordApprovalEvent({
      id: a.approvalId,
      ts: a.ts,
      threadId: a.threadId,
      messageId: a.messageId,
      agent: a.agent,
      tool: a.tool,
      preview: a.preview,
    });
    // Rehydrated approvals (thread loaded from disk) are listed but not
    // re-announced — the user already heard about them the first time.
    if (now - a.ts <= FRESH_APPROVAL_MS) announce(a, activeThreadId);
  }
  for (const id of Array.from(known.keys())) {
    if (currentIds.has(id)) continue;
    known.delete(id);
    clearApprovalEvent(id);
  }
}

function onFocusEvent(ev: Event): void {
  const detail = (
    ev as CustomEvent<{ threadId?: string; messageId?: string } | undefined>
  ).detail;
  if (detail?.threadId && detail?.messageId) {
    void focusApproval({
      threadId: detail.threadId,
      messageId: detail.messageId,
    });
    return;
  }
  focusOldestApproval();
}

/**
 * Subscribe to the store once per app lifetime. Idempotent; runs at module
 * load — `ActivityBar` (always mounted) imports this module, so the watcher
 * is live from the first frame. Exported so a future entry point can call it
 * explicitly should the import graph change.
 */
export function installAttentionWatcher(): void {
  if (watcherInstalled || typeof window === "undefined") return;
  watcherInstalled = true;
  const initial = useCortexStore.getState();
  diffApprovals(initial.threads, initial.activeThreadId);
  useCortexStore.subscribe((s, prev) => {
    // Cheap identity check — most store writes don't touch the thread list.
    if (s.threads === prev.threads) return;
    diffApprovals(s.threads, s.activeThreadId);
  });
  window.addEventListener(FOCUS_APPROVAL_EVENT, onFocusEvent);
}

installAttentionWatcher();
