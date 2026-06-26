import { useSyncExternalStore } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  recentAudit,
  recentCrashes,
  recentIssues,
  type AuditRow,
  type CrashRow,
  type IssueRow,
} from "@/lib/observability";
import {
  subscribeMonitorLines,
  type MonitorLinePayload,
} from "@/lib/monitors";
import {
  subscribeRepoWatcher,
  type RepoWatcherEvent,
} from "@/lib/repo-watcher";
import {
  subscribeConfigChanges,
  type ConfigChangedEvent,
} from "@/lib/config-watcher";

/**
 * Unified notification inbox. Aggregates events from:
 *   - `recent_crashes` (top 20)
 *   - `recent_issues`  (top 20)
 *   - `recent_audit`   (top 50)
 *   - `monitor-line`   Tauri events (accumulated locally)
 *   - `config-changed` Tauri events (accumulated locally)
 *   - `repo-watcher:event` Tauri events (accumulated locally)
 *
 * Pull sources (crashes/issues/audit) are re-polled every REFRESH_MS while
 * the center is open; push sources (monitor/config/repo) are streamed via
 * Tauri events that we subscribe to lazily on first activation. "Read" is
 * tracked in-memory only — by id — and resets when the app reloads.
 */

/** Notification source taxonomy. Drives filter chips and deep-link routing. */
export type NotifSource = "crash" | "issue" | "audit" | "monitor" | "config" | "repo" | "job";

/**
 * Completion/failure record for a long-running or background job (Cookbook
 * pull, Deep Research, Eval, Routine run). Pushed by `src/state/jobs.ts` when
 * a job settles — and by `src/lib/routines.ts` when a routine run is recorded
 * — so the outcome lands in the inbox regardless of which tab is open. `kind`
 * drives the deep-link back to the owning activity tab.
 */
export interface JobEventRow {
  ts: number;
  kind: "pull" | "research" | "eval" | "routine";
  label: string;
  ok: boolean;
  detail?: string | null;
}

/** Severity ladder. Drives the badge colour on the StatusBar 🔔 button. */
export type NotifSeverity = "info" | "warning" | "error";

export interface Notification {
  /** Stable id within the session — used for the read-set + React keys. */
  id: string;
  /** Unix epoch milliseconds. */
  ts: number;
  severity: NotifSeverity;
  source: NotifSource;
  /** Short, single-line summary. Full payload lives in `detail`. */
  message: string;
  /** Multi-line detail rendered under the message when present. */
  detail?: string | null;
  /** Optional source-specific reference (crash id, config path, etc.). */
  ref?: string | null;
}

/** Filter chip ids. "all" is the implicit default. */
export type NotifFilter = "all" | "errors" | "warnings" | NotifSource;

const PULL_LIMITS = {
  crashes: 20,
  issues: 20,
  audit: 50,
} as const;

const STREAM_CAP = 200; // hard cap on each in-memory event log
// Crash/issue/audit data changes on the order of minutes; a 5s process-wide
// poll of three backend commands was needless churn. 30s is plenty (live
// changes still arrive via the event streams in startStreams).
const REFRESH_MS = 30_000;

interface Listener {
  (): void;
}

/**
 * Internal store. Kept module-scoped so the StatusBar badge, the modal, and
 * the slash command all read the same data without prop drilling.
 */
const state: {
  crashes: CrashRow[];
  issues: IssueRow[];
  audit: AuditRow[];
  monitor: MonitorLinePayload[];
  config: ConfigChangedEvent[];
  repo: RepoWatcherEvent[];
  jobs: JobEventRow[];
  /** Read-status set keyed by Notification.id. */
  read: Set<string>;
  /** Live subscriber count for pull-refresh gating. */
  subscribers: number;
  /** Tauri event unsubscribers — populated on first activation. */
  unlisteners: UnlistenFn[];
  /** Pull-refresh interval handle. */
  pullTimer: number | null;
  /** Reactive listeners for `useNotifications`. */
  listeners: Set<Listener>;
} = {
  crashes: [],
  issues: [],
  audit: [],
  monitor: [],
  config: [],
  repo: [],
  jobs: [],
  read: new Set<string>(),
  subscribers: 0,
  unlisteners: [],
  pullTimer: null,
  listeners: new Set(),
};

/** Cached snapshot — `useSyncExternalStore` requires stable references when
 *  nothing changed, otherwise React tears every render. Reset by `notify`
 *  before fanning out to listeners. */
let snapshotCache: Notification[] | null = null;

function notify(): void {
  snapshotCache = null;
  for (const fn of state.listeners) {
    try {
      fn();
    } catch {
      /* listener errors are non-fatal */
    }
  }
}

/** Push the head of a bounded ring buffer. Mutates in place + returns it. */
function pushBounded<T>(buf: T[], item: T): T[] {
  buf.unshift(item);
  if (buf.length > STREAM_CAP) buf.length = STREAM_CAP;
  return buf;
}

/** Pull the three backend-backed sources concurrently. Best-effort: failures
 *  leave the previous snapshot untouched so transient backend hiccups don't
 *  blank the inbox. */
async function pullAll(): Promise<void> {
  const results = await Promise.allSettled([
    recentCrashes(PULL_LIMITS.crashes),
    recentIssues(PULL_LIMITS.issues),
    recentAudit(PULL_LIMITS.audit),
  ]);
  if (results[0].status === "fulfilled") state.crashes = results[0].value;
  if (results[1].status === "fulfilled") state.issues = results[1].value;
  if (results[2].status === "fulfilled") state.audit = results[2].value;
  notify();
}

/** Lazily attach Tauri event listeners + start the pull-refresh timer. Called
 *  once per app lifetime — subsequent `activate` calls just bump the refcount. */
async function startStreams(): Promise<void> {
  if (state.unlisteners.length > 0) return;
  try {
    const offMon = await subscribeMonitorLines((p) => {
      pushBounded(state.monitor, p);
      notify();
    });
    state.unlisteners.push(offMon);
  } catch {
    /* Tauri unavailable (dev preview) — skip the stream. */
  }
  try {
    const offCfg = await subscribeConfigChanges((p) => {
      pushBounded(state.config, p);
      notify();
    });
    state.unlisteners.push(offCfg);
  } catch {
    /* same */
  }
  try {
    const offRepo = await subscribeRepoWatcher((p) => {
      pushBounded(state.repo, p);
      notify();
    });
    state.unlisteners.push(offRepo);
  } catch {
    /* same */
  }
}

/** Begin pulling + streaming. Idempotent. Returns a deactivator. */
export function activateNotificationCenter(): () => void {
  state.subscribers += 1;
  if (state.subscribers === 1) {
    void startStreams();
    void pullAll();
    state.pullTimer = window.setInterval(() => void pullAll(), REFRESH_MS);
  }
  return () => {
    state.subscribers = Math.max(0, state.subscribers - 1);
    if (state.subscribers === 0 && state.pullTimer !== null) {
      window.clearInterval(state.pullTimer);
      state.pullTimer = null;
    }
    // We intentionally keep the Tauri listeners + the in-memory log around
    // even when no UI is mounted — otherwise we'd drop monitor/config/repo
    // events that fire while the modal is closed.
  };
}

/** Force-refresh the pull sources. Used by the modal's "Refresh" button. */
export async function refreshNotificationCenter(): Promise<void> {
  await pullAll();
}

/**
 * Record a settled long-running job (Cookbook pull / Deep Research / Eval).
 * Called by the global job store so completion is visible from ANY tab — the
 * whole point of the job-store work; a pull that finishes while the user is
 * in the editor still lands here and lights the StatusBar bell.
 */
export function recordJobEvent(evt: Omit<JobEventRow, "ts"> & { ts?: number }): void {
  pushBounded(state.jobs, { ts: evt.ts ?? Date.now(), ...evt });
  notify();
}

/** Current notification list, outside React. Used by the E2E probe. */
export function getNotificationsSnapshot(): Notification[] {
  return getSnapshot();
}

// ---------------------------------------------------------------------------
// Mapping: raw rows / events → Notification view-model
// ---------------------------------------------------------------------------

function crashSeverity(kind: string): NotifSeverity {
  const k = kind.toLowerCase();
  if (k.includes("panic") || k.includes("unhandled")) return "error";
  if (k.includes("warn")) return "warning";
  return "error";
}

function auditSeverity(action: string): NotifSeverity {
  const a = action.toLowerCase();
  if (a.includes("deny") || a.includes("fail") || a.includes("error")) return "warning";
  return "info";
}

function monitorSeverity(level: MonitorLinePayload["level"]): NotifSeverity {
  if (level === "error") return "error";
  if (level === "warn") return "warning";
  return "info";
}

function truncate(s: string, max = 160): string {
  if (s.length <= max) return s;
  return `${s.slice(0, max - 1)}…`;
}

function buildAll(): Notification[] {
  const out: Notification[] = [];

  for (const r of state.crashes) {
    out.push({
      id: `crash:${r.id}`,
      ts: r.ts,
      severity: crashSeverity(r.kind),
      source: "crash",
      message: `${r.kind}: ${truncate(r.message)}`,
      detail: r.stack ?? null,
      ref: String(r.id),
    });
  }
  for (const r of state.issues) {
    out.push({
      id: `issue:${r.fingerprint}`,
      ts: r.last_seen,
      severity: "warning",
      source: "issue",
      message: `${r.error_class ?? "issue"} ×${r.count}: ${truncate(r.message)}`,
      detail: r.agent_id ? `agent ${r.agent_id}` : null,
      ref: r.fingerprint,
    });
  }
  for (const [i, r] of state.audit.entries()) {
    out.push({
      id: `audit:${i}:${r.ts}:${r.action}:${r.session_id ?? ""}:${r.agent_id ?? ""}`,
      ts: r.ts,
      severity: auditSeverity(r.action),
      source: "audit",
      message: `${r.action}${r.detail ? `: ${truncate(r.detail)}` : ""}`,
      detail: r.detail ?? null,
      ref: r.session_id,
    });
  }
  for (const p of state.monitor) {
    out.push({
      id: `monitor:${p.ts}:${p.name}:${p.line.slice(0, 32)}`,
      ts: p.ts,
      severity: monitorSeverity(p.level),
      source: "monitor",
      message: `[${p.name}] ${truncate(p.line)}`,
      detail: null,
      ref: p.name,
    });
  }
  for (const p of state.config) {
    out.push({
      id: `config:${p.ts}:${p.path}`,
      ts: p.ts,
      severity: "info",
      source: "config",
      message: `config ${p.kind}: ${p.path.split("/").pop() ?? p.path}`,
      detail: p.path,
      ref: p.path,
    });
  }
  for (const p of state.repo) {
    out.push({
      id: `repo:${p.ts}:${p.path}`,
      ts: p.ts,
      severity: "info",
      source: "repo",
      message: `repo ${p.kind}: ${p.path.split("/").pop() ?? p.path}`,
      detail: p.path,
      ref: p.path,
    });
  }
  for (const j of state.jobs) {
    out.push({
      id: `job:${j.ts}:${j.kind}:${j.label}`,
      ts: j.ts,
      severity: j.ok ? "info" : "error",
      source: "job",
      message: j.ok ? `${j.label} finished` : `${j.label} failed`,
      detail: j.detail ?? null,
      ref: j.kind,
    });
  }

  // Newest first; deterministic tie-break on id so React keys stay stable.
  out.sort((a, b) => (b.ts - a.ts) || a.id.localeCompare(b.id));
  return out;
}

function getSnapshot(): Notification[] {
  if (!snapshotCache) snapshotCache = buildAll();
  return snapshotCache;
}
function subscribe(fn: Listener): () => void {
  state.listeners.add(fn);
  return () => state.listeners.delete(fn);
}

/** React hook — returns the live notification list. */
export function useNotifications(): Notification[] {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/** React hook — returns just the unread count + the highest unread severity.
 *  Cheap enough to call from the StatusBar on every render. */
export function useUnread(): { count: number; severity: NotifSeverity | null } {
  const all = useNotifications();
  let count = 0;
  let highest: NotifSeverity | null = null;
  for (const n of all) {
    if (state.read.has(n.id)) continue;
    count += 1;
    if (n.severity === "error") highest = "error";
    else if (n.severity === "warning" && highest !== "error") highest = "warning";
    else if (!highest) highest = n.severity;
  }
  return { count, severity: highest };
}

/** Mark a single notification as read. */
export function markRead(id: string): void {
  if (!state.read.has(id)) {
    state.read.add(id);
    notify();
  }
}

/** Mark every currently-known notification as read. */
export function markAllRead(): void {
  const all = buildAll();
  let touched = false;
  for (const n of all) {
    if (!state.read.has(n.id)) {
      state.read.add(n.id);
      touched = true;
    }
  }
  if (touched) notify();
}

/** Is `id` in the read-set? Pure read, no subscription. */
export function isRead(id: string): boolean {
  return state.read.has(id);
}

// ---------------------------------------------------------------------------
// Deep-link dispatcher
// ---------------------------------------------------------------------------

/** Open the appropriate viewer for `n`. Marks it read as a side-effect. */
export async function openNotification(n: Notification): Promise<void> {
  markRead(n.id);
  try {
    switch (n.source) {
      case "crash": {
        const { openCrashViewer } = await import("@/lib/crash-viewer");
        await openCrashViewer();
        return;
      }
      case "issue":
      case "audit": {
        const { openAuditLogPanel } = await import("@/components/AuditLogPanel");
        openAuditLogPanel();
        return;
      }
      case "config": {
        const { openSchemaEditor } = await import("@/components/SchemaEditor");
        openSchemaEditor();
        return;
      }
      case "monitor": {
        // Collapse the activity panel → chat reclaims the viewport.
        const { useCortexStore } = await import("@/state/store");
        useCortexStore.getState().setActivityTab(null);
        return;
      }
      case "repo": {
        // Repo events don't have a dedicated viewer — surface in the audit
        // log so the user can scan recent file activity in context.
        const { openAuditLogPanel } = await import("@/components/AuditLogPanel");
        openAuditLogPanel();
        return;
      }
      case "job": {
        // Deep-link back to the activity tab that owns this job kind (`ref`
        // carries the kind — see buildAll).
        const { useCortexStore } = await import("@/state/store");
        const tab =
          n.ref === "research"
            ? "research"
            : n.ref === "eval"
              ? "eval"
              : n.ref === "routine"
                ? "routines"
                : "cookbook";
        useCortexStore.getState().setActivityTab(tab);
        return;
      }
    }
  } catch {
    /* deep-link target failed to mount — already marked read, nothing else
     *  the inbox can usefully do here. */
  }
}

// ---------------------------------------------------------------------------
// Imperative summoner for the modal (`/notifs` slash command + 🔔 click)
// ---------------------------------------------------------------------------

export async function openNotificationCenter(): Promise<void> {
  const { mountNotificationCenter } = await import(
    "@/components/NotificationCenter"
  );
  mountNotificationCenter();
}
