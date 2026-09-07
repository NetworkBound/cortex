import { useEffect, useRef, useState } from "react";
import { getModels, postUltimate } from "../lib/api";
import { useWs } from "../lib/useWs";
import { useStore } from "../lib/store";
import { useStickToBottom } from "../lib/scroll";
import Markdown from "../components/Markdown";
import type { PlannedSubtask, UltEvent, WsFrameBase } from "../lib/types";

interface ModelRun {
  model: string;
  ok?: boolean;
  output?: string;
}

interface SubtaskState {
  id: string;
  task: string;
  models: ModelRun[];
  merged?: string;
}

interface RunState {
  runId: string;
  goal: string;
  plan?: PlannedSubtask[];
  subtasks: Map<string, SubtaskState>;
  synthesis?: string;
  cost?: number;
  done?: boolean;
  error?: string;
}

// An Ultimate run fans out across several models, so frames can be sparse; give
// it a generous window before the watchdog assumes the stream died.
const RUN_WATCHDOG_MS = 90_000;

export default function UltimateView() {
  const { activeProjectRoot, wsStatus } = useStore();
  const [goal, setGoal] = useState("");
  const [fanOut, setFanOut] = useState(3);
  const [leadModel, setLeadModel] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [running, setRunning] = useState(false);
  const [run, setRun] = useState<RunState | null>(null);
  const runIdRef = useRef<string | null>(null);
  const watchdog = useRef<ReturnType<typeof setTimeout> | null>(null);
  const { ref: scrollRef, notify } = useStickToBottom<HTMLDivElement>();

  useEffect(() => {
    getModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  useEffect(notify, [run, notify]);

  const mutate = (fn: (r: RunState) => RunState) =>
    setRun((r) => (r ? fn(r) : r));

  const clearWatchdog = () => {
    if (watchdog.current) {
      clearTimeout(watchdog.current);
      watchdog.current = null;
    }
  };

  const finishRun = () => {
    clearWatchdog();
    setRunning(false);
  };

  // (Re)arm the run watchdog; every Ultimate frame resets it. If it fires we
  // assume the stream dropped and unlock the Run button + surface an error so
  // it never stays stuck on "Running…".
  const armWatchdog = () => {
    clearWatchdog();
    watchdog.current = setTimeout(() => {
      setRun((r) =>
        r && !r.done
          ? {
              ...r,
              done: true,
              error:
                "Connection lost — stopped receiving updates. The run may still be finishing on the server.",
            }
          : r,
      );
      finishRun();
    }, RUN_WATCHDOG_MS);
  };

  useEffect(() => clearWatchdog, []);

  useWs((f: WsFrameBase) => {
    if (f.run_id !== runIdRef.current) return;
    if (running) armWatchdog();

    if (f.type === "ultimate") {
      const ev = (f.event as UltEvent) || ({} as UltEvent);
      applyUltEvent(mutate, ev);
    } else if (f.type === "ultimate_done") {
      const result = (f.result as { final_output?: string; total_usd?: number }) || {};
      mutate((r) => ({
        ...r,
        synthesis: r.synthesis ?? result.final_output,
        cost: result.total_usd ?? r.cost,
        done: true,
      }));
      finishRun();
    } else if (f.type === "ultimate_error") {
      mutate((r) => ({ ...r, error: f.message as string, done: true }));
      finishRun();
    }
  });

  const start = async () => {
    const g = goal.trim();
    if (!g || running) return;
    setRunning(true);
    setRun(null);
    runIdRef.current = null;
    armWatchdog();

    try {
      // The POST resolves only when the whole run finishes, but progress streams
      // over WS in the meantime — so we kick it off and rely on the WS frames.
      const promise = postUltimate({
        goal: g,
        project_root: activeProjectRoot,
        fan_out: fanOut,
        lead_model: leadModel || undefined,
      });

      // The server publishes ultimate frames tagged with the run_id it returns;
      // but we don't have it until the POST resolves. To capture early frames we
      // accept ANY ultimate run while none is pinned, then pin on first frame.
      const res = await promise;
      runIdRef.current = res.run_id;
      // Backfill the final result in case WS frames were missed.
      mutate((r) => ({
        ...r,
        runId: res.run_id,
        synthesis: r.synthesis ?? res.result?.final_output,
        cost: res.result?.total_usd ?? r.cost,
        done: true,
      }));
      finishRun();
    } catch (e) {
      setRun((r) => ({
        runId: runIdRef.current ?? "",
        goal: g,
        subtasks: r?.subtasks ?? new Map(),
        ...r,
        error: e instanceof Error ? e.message : String(e),
        done: true,
      }));
      finishRun();
    }
  };

  // Stop waiting on the run. The backend keeps going, but the UI is freed and
  // the timeline marked done so the Run button never stays stuck.
  const stop = () => {
    mutate((r) => ({ ...r, done: true }));
    finishRun();
  };

  // Bootstrap: since the run_id only arrives when POST resolves, capture the
  // first ultimate frame's run_id eagerly so the live timeline isn't empty.
  useWs((f: WsFrameBase) => {
    if (!running) return;
    if (runIdRef.current) return;
    if (f.type === "ultimate" || f.type === "ultimate_done" || f.type === "ultimate_error") {
      runIdRef.current = f.run_id as string;
      setRun({ runId: f.run_id as string, goal, subtasks: new Map() });
      armWatchdog();
      // re-deliver this frame
      if (f.type === "ultimate") applyUltEvent(mutate, (f.event as UltEvent) || ({} as UltEvent));
    }
  });

  return (
    <>
      <div className="scroll" ref={scrollRef}>
        <div className="pad">
          <div className="field">
            <label>Goal</label>
            <textarea
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="Describe what you want accomplished. Cortex decomposes it and races your connected models."
              disabled={running}
            />
          </div>
          <div className="row-2">
            <div className="field">
              <label>Fan-out</label>
              <input
                type="number"
                min={1}
                max={9}
                value={fanOut}
                onChange={(e) => setFanOut(Math.max(1, Number(e.target.value) || 1))}
                disabled={running}
              />
            </div>
            <div className="field">
              <label>Lead model</label>
              <select
                className="model-select"
                style={{ width: "100%" }}
                value={leadModel}
                onChange={(e) => setLeadModel(e.target.value)}
                disabled={running}
              >
                <option value="">Auto (strongest)</option>
                {models.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </select>
            </div>
          </div>
          {models.length > 0 && (
            <div className="faint" style={{ fontSize: 12, marginBottom: 10 }}>
              {models.length} models connected — all eligible for the race.
            </div>
          )}
          {running && wsStatus !== "open" && (
            <div
              className="banner reconnecting"
              role="status"
              aria-live="polite"
              style={{ margin: "0 0 10px" }}
            >
              <span className="spin" aria-hidden="true" />
              Reconnecting… progress will resume when the link is back.
            </div>
          )}
          {running ? (
            <button
              className="btn"
              style={{ width: "100%" }}
              onClick={stop}
              aria-label="Stop the run"
            >
              ◼ Stop
            </button>
          ) : (
            <button
              className="btn primary"
              style={{ width: "100%" }}
              onClick={start}
              disabled={!goal.trim()}
              aria-label="Run Ultimate"
            >
              Run Ultimate
            </button>
          )}
        </div>

        {run && <Timeline run={run} />}
      </div>
    </>
  );
}

function applyUltEvent(mutate: (fn: (r: RunState) => RunState) => void, ev: UltEvent) {
  switch (ev.type) {
    case "plan":
      mutate((r) => ({ ...r, plan: (ev.subtasks as PlannedSubtask[]) || [] }));
      break;
    case "subtask_started":
      mutate((r) => {
        const subtasks = new Map(r.subtasks);
        subtasks.set(ev.id as string, {
          id: ev.id as string,
          task: ev.task as string,
          models: ((ev.models as string[]) || []).map((m) => ({ model: m })),
        });
        return { ...r, subtasks };
      });
      break;
    case "model_done":
      mutate((r) => {
        const subtasks = new Map(r.subtasks);
        const st = subtasks.get(ev.subtask_id as string);
        if (st) {
          const models = st.models.map((mr) =>
            mr.model === ev.model
              ? { ...mr, ok: ev.ok as boolean, output: ev.output as string }
              : mr,
          );
          if (!models.some((mr) => mr.model === ev.model)) {
            models.push({
              model: ev.model as string,
              ok: ev.ok as boolean,
              output: ev.output as string,
            });
          }
          subtasks.set(st.id, { ...st, models });
        }
        return { ...r, subtasks };
      });
      break;
    case "subtask_merged":
      mutate((r) => {
        const subtasks = new Map(r.subtasks);
        const st = subtasks.get(ev.id as string);
        if (st) subtasks.set(st.id, { ...st, merged: ev.merged as string });
        return { ...r, subtasks };
      });
      break;
    case "synthesis":
      mutate((r) => ({ ...r, synthesis: ev.merged as string }));
      break;
    case "cost":
      mutate((r) => ({ ...r, cost: ev.usd as number }));
      break;
    case "done":
      mutate((r) => ({ ...r, done: true }));
      break;
    case "error":
      mutate((r) => ({ ...r, error: ev.msg as string, done: true }));
      break;
  }
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard may be unavailable (insecure origin) — fail quietly */
    }
  };
  return (
    <button
      className="msg-copy"
      onClick={copy}
      aria-label={copied ? "Copied" : "Copy synthesis"}
      title={copied ? "Copied" : "Copy"}
    >
      {copied ? "✓ Copied" : "⧉ Copy"}
    </button>
  );
}

function Timeline({ run }: { run: RunState }) {
  const subtasks = [...run.subtasks.values()];
  return (
    <div className="timeline">
      {run.plan && (
        <div className="card">
          <div className="card-title">
            🗺 Plan
            <span className="badge-soft">{run.plan.length} subtasks</span>
          </div>
          {run.plan.map((s) => (
            <div key={s.id} className="subtask">
              <div className="head">
                <strong>{s.id}</strong>
                {s.fan_out && <span className="badge-soft">fan-out</span>}
                <span className="chip">{s.kind}</span>
                <span className="chip">{s.difficulty}</span>
              </div>
              <div style={{ marginTop: 4 }}>{s.task}</div>
            </div>
          ))}
        </div>
      )}

      {subtasks.map((st) => (
        <div key={st.id} className="card">
          <div className="card-title">
            ⚙ {st.id}
          </div>
          <div className="muted" style={{ fontSize: 13 }}>{st.task}</div>
          <div className="model-race">
            {st.models.map((mr) => (
              <span
                key={mr.model}
                className={`model-pill ${mr.ok === undefined ? "" : mr.ok ? "done" : "fail"}`}
              >
                {mr.ok === undefined ? <span className="spin" /> : mr.ok ? "✓" : "✗"}
                {mr.model}
              </span>
            ))}
          </div>
          {st.models
            .filter((mr) => mr.output)
            .map((mr) => (
              <details className="model-out" key={mr.model}>
                <summary>{mr.model} output</summary>
                <div className="body">
                  <Markdown>{mr.output as string}</Markdown>
                </div>
              </details>
            ))}
          {st.merged && (
            <details className="model-out" open>
              <summary>merged result</summary>
              <div className="body">
                <Markdown>{st.merged}</Markdown>
              </div>
            </details>
          )}
        </div>
      ))}

      {run.synthesis && (
        <div className="card">
          <div className="card-title">
            ✦ Final synthesis
            {typeof run.cost === "number" && (
              <span className="badge-soft cost-badge">${run.cost.toFixed(4)}</span>
            )}
            <span className="spacer" style={{ flex: 1 }} />
            <CopyButton text={run.synthesis} />
          </div>
          <Markdown>{run.synthesis}</Markdown>
        </div>
      )}

      {!run.synthesis && typeof run.cost === "number" && (
        <div className="chip cost-badge">cost ${run.cost.toFixed(4)}</div>
      )}

      {run.error && <div className="banner err">{run.error}</div>}
    </div>
  );
}
