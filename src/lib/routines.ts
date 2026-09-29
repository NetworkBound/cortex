// Scheduled agents ("Routines") — frontend bindings.
//
// Mirrors `src-tauri/src/commands/routines.rs`. A routine is a saved prompt
// that runs on an interval (or manually). The backend scheduler fires due
// routines; every run (manual or scheduled) emits `routines:ran` (legacy id
// ping) and `routines:run-recorded` (the full run record).
//
// `initRoutineNotifications` is the module-scope bridge that makes routine
// outcomes visible from ANY tab: it forwards every recorded run into the
// NotificationCenter inbox and toasts scheduled failures. It lives here (not
// in the panel) precisely so background runs aren't invisible when the
// Routines tab is closed — the audit's core finding.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { recordJobEvent } from "@/lib/notification-center";
import { pushToast } from "@/lib/toast";

export interface RoutineSpec {
  id: string;
  name: string;
  prompt: string;
  interval_minutes: number;
  enabled: boolean;
  last_run_unix_ms: number;
  last_status: string;
  last_output: string;
  last_error: string;
  /** Registry id of the adapter to run through (`claude-cli`, `codex-cli`,
   *  …). `null` = auto: the Cortex Gateway when configured, else the first
   *  available local agent. */
  agent_id?: string | null;
  /** Working directory for CLI adapters (a project root). */
  project_root?: string | null;
  /** Daily wall-clock time `"HH:MM"` (24h, local). Takes precedence over
   *  `interval_minutes` — send `interval_minutes: 0` alongside it. */
  daily_at?: string | null;
  /** Output-only: unix ms of the next scheduled run (`null` = manual-only or
   *  disabled). Recomputed by the backend on every list. */
  next_run_unix_ms?: number | null;
}

export function emptyRoutine(): RoutineSpec {
  return {
    id: "",
    name: "",
    prompt: "",
    interval_minutes: 60,
    enabled: true,
    last_run_unix_ms: 0,
    last_status: "",
    last_output: "",
    last_error: "",
    agent_id: null,
    project_root: null,
    daily_at: null,
    next_run_unix_ms: null,
  };
}

/** Human "in 5m" / "in 2h 10m" / "due now" for a future unix-ms instant. */
export function timeUntil(ts: number | null | undefined): string {
  if (!ts) return "";
  const s = Math.round((ts - Date.now()) / 1000);
  if (s <= 30) return "due now";
  if (s < 3600) return `in ${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86400) {
    const h = Math.floor(s / 3600);
    const m = Math.round((s % 3600) / 60);
    return m > 0 ? `in ${h}h ${m}m` : `in ${h}h`;
  }
  const d = Math.floor(s / 86400);
  const h = Math.round((s % 86400) / 3600);
  return h > 0 ? `in ${d}d ${h}h` : `in ${d}d`;
}

export async function listRoutines(): Promise<RoutineSpec[]> {
  return invoke<RoutineSpec[]>("list_routines");
}

export async function saveRoutine(
  routine: RoutineSpec,
): Promise<RoutineSpec[]> {
  return invoke<RoutineSpec[]>("save_routine", { routine });
}

export async function deleteRoutine(id: string): Promise<RoutineSpec[]> {
  return invoke<RoutineSpec[]>("delete_routine", { id });
}

export async function setRoutineEnabled(
  id: string,
  enabled: boolean,
): Promise<RoutineSpec[]> {
  return invoke<RoutineSpec[]>("set_routine_enabled", { id, enabled });
}

export async function runRoutineNow(id: string): Promise<RoutineSpec> {
  return invoke<RoutineSpec>("run_routine_now", { id });
}

export async function onRoutineRan(
  cb: (id: string) => void,
): Promise<UnlistenFn> {
  return listen<string>("routines:ran", (e) => cb(e.payload));
}

/** One completed run — mirrors `RoutineRun` in routines.rs (newest first). */
export interface RoutineRun {
  run_id: string;
  routine_id: string;
  routine_name: string;
  prompt: string;
  started_unix_ms: number;
  duration_ms: number;
  status: string; // "ok" | "error"
  output: string;
  error: string;
  trigger: string; // "manual" | "scheduled"
}

export async function listRoutineRuns(
  routineId?: string,
): Promise<RoutineRun[]> {
  return invoke<RoutineRun[]>("list_routine_runs", {
    routineId: routineId ?? null,
    limit: null,
  });
}

/**
 * Materialize a run as a chat session (prompt = user turn, output = assistant
 * turn) and return its session id — feed it to `cortex:chat-replay` to open.
 */
export async function routineRunAsSession(runId: string): Promise<string> {
  return invoke<string>("routine_run_as_session", { runId });
}

export async function onRoutineRunRecorded(
  cb: (run: RoutineRun) => void,
): Promise<UnlistenFn> {
  return listen<RoutineRun>("routines:run-recorded", (e) => cb(e.payload));
}

let notificationsArmed = false;

/**
 * One-time boot hookup (called from App.tsx, which is always mounted —
 * same pattern as `initJobStore`). Every recorded run lands in the
 * NotificationCenter inbox; scheduled failures also toast (the backend
 * additionally fires an OS notification for those). Manual runs are NOT
 * toasted here — the panel the user just clicked in already does that.
 */
export function initRoutineNotifications(): void {
  if (notificationsArmed) return;
  notificationsArmed = true;
  void onRoutineRunRecorded((run) => {
    const ok = run.status === "ok";
    recordJobEvent({
      kind: "routine",
      label: `Routine “${run.routine_name}” (${run.trigger})`,
      ok,
      detail: ok ? run.output.slice(0, 400) : run.error.slice(0, 400),
    });
    if (!ok && run.trigger === "scheduled") {
      pushToast({
        title: `Routine “${run.routine_name}” failed`,
        body: run.error.slice(0, 200),
        kind: "error",
      });
    }
  }).catch(() => {
    /* Tauri unavailable (dev preview) — notifications stay panel-local */
  });
}
