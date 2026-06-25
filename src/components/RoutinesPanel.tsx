/**
 * Scheduled agents ("Routines") panel.
 *
 * Create a routine (a name + a task prompt + a cadence) and the backend
 * scheduler runs it on that interval through the Cortex Gateway, recording
 * each run into a persistent per-run history. Routines can be enabled/
 * disabled, run on demand, and deleted. Each routine's history is browsable
 * inline — every run shows status, trigger, duration and output, and can be
 * reopened as a chat session ("Open as chat") to continue the conversation.
 * Refreshes when the backend emits `routines:ran`.
 *
 * Bindings live in `src/lib/routines.ts`; outcome→NotificationCenter
 * forwarding is module-scope there (armed by the StatusBar), NOT here — the
 * whole point is that runs stay visible when this panel is closed.
 */

import { useCallback, useEffect, useState } from "react";
import { Play, Trash2, Plus, Clock, History, MessageSquare } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import {
  listRoutines,
  saveRoutine,
  deleteRoutine,
  setRoutineEnabled,
  runRoutineNow,
  onRoutineRan,
  listRoutineRuns,
  routineRunAsSession,
  emptyRoutine,
  type RoutineSpec,
  type RoutineRun,
} from "@/lib/routines";
import "../styles/routines.css";

const CADENCES: { minutes: number; label: string }[] = [
  { minutes: 0, label: "Manual only" },
  { minutes: 15, label: "Every 15 min" },
  { minutes: 30, label: "Every 30 min" },
  { minutes: 60, label: "Hourly" },
  { minutes: 360, label: "Every 6 hours" },
  { minutes: 1440, label: "Daily" },
];

function cadenceLabel(min: number): string {
  return CADENCES.find((c) => c.minutes === min)?.label ?? `Every ${min} min`;
}

const ago = (ms: number): string => timeAgo(ms, { empty: "never" });

function fmtDuration(ms: number): string {
  if (ms < 1000) return "<1s";
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
}

/**
 * Per-routine run history. Fetches lazily on mount (the parent only mounts it
 * for the expanded routine) and re-fetches when `version` bumps (the parent
 * increments it on every `routines:ran`).
 */
function RoutineHistory({ routineId, version }: { routineId: string; version: number }) {
  const [runs, setRuns] = useState<RoutineRun[] | null>(null);
  const [openRun, setOpenRun] = useState<string | null>(null);
  const [opening, setOpening] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    listRoutineRuns(routineId)
      .then((rows) => { if (live) setRuns(rows); })
      .catch(() => { if (live) setRuns([]); });
    return () => { live = false; };
  }, [routineId, version]);

  const openAsChat = useCallback(async (run: RoutineRun) => {
    setOpening(run.run_id);
    try {
      const sessionId = await routineRunAsSession(run.run_id);
      window.dispatchEvent(
        new CustomEvent("cortex:chat-replay", { detail: { session_id: sessionId } }),
      );
      // Collapse the activity panel so the chat (now showing the run) is
      // front and center — same reveal the notification deep-link uses.
      useCortexStore.getState().setActivityTab(null);
      pushToast({ title: "Run opened in chat", body: "Reply to continue from this result.", kind: "success" });
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    } finally {
      setOpening(null);
    }
  }, []);

  if (runs === null) return <p className="routines-history-empty">Loading history…</p>;
  if (runs.length === 0) {
    return <p className="routines-history-empty">No runs recorded yet. Run it now or wait for the schedule.</p>;
  }
  return (
    <ul className="routines-history-list">
      {runs.map((run) => (
        <li key={run.run_id} className="routines-run">
          <div className="routines-run-head">
            <span
              className={`routines-run-dot ${run.status === "ok" ? "ok" : "err"}`}
              aria-label={run.status === "ok" ? "succeeded" : "failed"}
            />
            <span className="routines-run-when">{ago(run.started_unix_ms)}</span>
            <span className="routines-run-meta">
              {run.trigger} · {fmtDuration(run.duration_ms)}
            </span>
            <button
              className="routines-act"
              onClick={() => setOpenRun((cur) => (cur === run.run_id ? null : run.run_id))}
            >
              {openRun === run.run_id ? "Hide output" : "Output"}
            </button>
            <button
              className="routines-act"
              disabled={opening === run.run_id}
              title="Open this run as a chat session and continue the conversation"
              onClick={() => void openAsChat(run)}
            >
              <MessageSquare size={12} strokeWidth={1.9} aria-hidden="true" />
              {opening === run.run_id ? "Opening…" : "Open as chat"}
            </button>
          </div>
          {openRun === run.run_id && (
            <pre className="routines-run-output">{run.status === "ok" ? run.output : run.error}</pre>
          )}
        </li>
      ))}
    </ul>
  );
}

export function RoutinesPanel() {
  const [routines, setRoutines] = useState<RoutineSpec[]>([]);
  const [draft, setDraft] = useState<RoutineSpec>(emptyRoutine());
  const [showForm, setShowForm] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  // Which routine's run history is expanded, and a version counter that
  // re-fetches it whenever any run completes (manual or scheduled).
  const [historyFor, setHistoryFor] = useState<string | null>(null);
  const [runsVersion, setRunsVersion] = useState(0);

  const reload = useCallback(async () => {
    try {
      setRoutines(await listRoutines());
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    let un: (() => void) | undefined;
    void onRoutineRan(() => {
      void reload();
      setRunsVersion((v) => v + 1);
    }).then((u) => (un = u));
    return () => un?.();
  }, [reload]);

  const onSave = useCallback(async () => {
    try {
      setRoutines(await saveRoutine(draft));
      setDraft(emptyRoutine());
      setShowForm(false);
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    }
  }, [draft]);

  const onRun = useCallback(async (id: string) => {
    setBusy(id);
    try {
      // The returned spec carries the run outcome — an LLM failure is recorded
      // as status "error", not thrown, so toast from the status (pre-fix this
      // said "Routine ran" even when the run failed).
      const spec = await runRoutineNow(id);
      await reload();
      if (spec.last_status === "ok") {
        pushToast({ title: "Routine ran", kind: "success" });
      } else {
        pushToast({ title: "Routine failed", body: spec.last_error.slice(0, 200), kind: "error" });
      }
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    } finally {
      setBusy(null);
    }
  }, [reload]);

  const onToggle = useCallback(async (r: RoutineSpec) => {
    try {
      setRoutines(await setRoutineEnabled(r.id, !r.enabled));
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    }
  }, []);

  const onDelete = useCallback(async (id: string) => {
    try {
      setRoutines(await deleteRoutine(id));
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    }
  }, []);

  return (
    <div className="routines-panel">
      <div className="routines-head">
        <button className="routines-new-btn" onClick={() => setShowForm((s) => !s)}>
          <Plus size={14} strokeWidth={2} aria-hidden="true" /> New routine
        </button>
      </div>

      {showForm && (
        <div className="routines-form">
          <input
            className="routines-input"
            placeholder="Name — e.g. “Morning service health check”"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
          <textarea
            className="routines-textarea"
            placeholder="Task prompt the agent should run on this schedule…"
            rows={3}
            value={draft.prompt}
            onChange={(e) => setDraft({ ...draft, prompt: e.target.value })}
          />
          <div className="routines-form-row">
            <label className="routines-cadence">
              <Clock size={13} strokeWidth={1.75} aria-hidden="true" />
              <select
                value={draft.interval_minutes}
                onChange={(e) => setDraft({ ...draft, interval_minutes: Number(e.target.value) })}
              >
                {CADENCES.map((c) => (
                  <option key={c.minutes} value={c.minutes}>{c.label}</option>
                ))}
              </select>
            </label>
            <label className="routines-enabled-check">
              <input
                type="checkbox"
                checked={draft.enabled}
                onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
              />
              enabled
            </label>
            <button className="routines-save-btn" onClick={() => void onSave()}>Save</button>
          </div>
        </div>
      )}

      {routines.length === 0 ? (
        <p className="routines-hint">No routines yet. Create one to run an agent task on a schedule.</p>
      ) : (
        <ul className="routines-list">
          {routines.map((r) => (
            <li key={r.id} className={`routines-row ${r.enabled ? "" : "routines-row-off"}`}>
              <div className="routines-row-head">
                <span className="routines-name">{r.name}</span>
                <span className="routines-cadence-tag">{cadenceLabel(r.interval_minutes)}</span>
                {r.last_status && (
                  <span className={`routines-status routines-status-${r.last_status}`}>
                    {r.last_status} · {ago(r.last_run_unix_ms)}
                  </span>
                )}
              </div>
              <div className="routines-row-actions">
                <label className="routines-toggle" title={r.enabled ? "Disable" : "Enable"}>
                  <input type="checkbox" checked={r.enabled} onChange={() => void onToggle(r)} />
                  <span />
                </label>
                <button
                  className="routines-act"
                  disabled={busy === r.id}
                  title="Run now"
                  onClick={() => void onRun(r.id)}
                >
                  <Play size={13} strokeWidth={1.9} aria-hidden="true" />
                  {busy === r.id ? "Running…" : "Run"}
                </button>
                <button
                  className={`routines-act ${historyFor === r.id ? "routines-act-active" : ""}`}
                  title="Browse this routine's recorded runs"
                  onClick={() => setHistoryFor((cur) => (cur === r.id ? null : r.id))}
                >
                  <History size={13} strokeWidth={1.75} aria-hidden="true" />
                  History
                </button>
                <button className="routines-act routines-act-danger" title="Delete" onClick={() => void onDelete(r.id)}>
                  <Trash2 size={13} strokeWidth={1.75} aria-hidden="true" />
                </button>
              </div>
              {historyFor === r.id && (
                <div className="routines-history">
                  <RoutineHistory routineId={r.id} version={runsVersion} />
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
