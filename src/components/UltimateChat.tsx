import { useCallback, useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { humanizeError } from "@/lib/errors";
import {
  subscribeUltimate,
  ultimateListModels,
  ultimateRun,
  type UltimateEvent,
  type UltimateSubtask,
} from "@/lib/cortex-bridge";
import { MarkdownView } from "./MarkdownView";
import "../styles/ultimate.css";

/** One model's attempt at a subtask, as it streams back via `model_done`. */
interface ModelResult {
  model: string;
  ok: boolean;
  output: string;
}

/** Live, UI-side view of a planned subtask, folded together from the
 *  `plan` / `subtask_started` / `model_done` / `subtask_merged` events. */
interface SubtaskState {
  id: string;
  task: string;
  kind?: string;
  difficulty?: string;
  fanOut?: boolean;
  /** Models racing on this subtask (from `subtask_started`). */
  models: string[];
  /** Per-model outputs (from `model_done`). */
  results: ModelResult[];
  /** Merged winner (from `subtask_merged`). */
  merged: string | null;
}

/**
 * The "Ultimate" multi-model agent.
 *
 * Combines every connected model: the lead model plans a goal into subtasks,
 * several models race on each subtask, the winners are merged per subtask, and
 * a final synthesis is composed. The whole run streams live over the
 * `ultimate:event` channel so the timeline below fills in as it happens —
 * the plan, the models racing, each model's output, the per-subtask merge, the
 * final synthesis, and the running cost.
 */
export function UltimateChat() {
  const [goal, setGoal] = useState("");
  const [projectRoot, setProjectRoot] = useState<string>(() => {
    try {
      return localStorage.getItem("cortex.ultimate.projectRoot") || "";
    } catch {
      return "";
    }
  });
  const [fanOut, setFanOut] = useState<number>(() => {
    try {
      const raw = Number(localStorage.getItem("cortex.ultimate.fanOut"));
      return Number.isFinite(raw) && raw >= 1 ? raw : 3;
    } catch {
      return 3;
    }
  });

  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Live timeline state, folded from the event stream.
  const [subtasks, setSubtasks] = useState<SubtaskState[]>([]);
  const [synthesis, setSynthesis] = useState<string | null>(null);
  const [costUsd, setCostUsd] = useState<number | null>(null);

  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    ultimateListModels()
      .then((m) => {
        if (mounted.current) setModels(m);
      })
      .catch((e) => {
        if (mounted.current) setError(humanizeError(e));
      })
      .finally(() => {
        if (mounted.current) setModelsLoading(false);
      });
    return () => {
      mounted.current = false;
    };
  }, []);

  const handleEvent = useCallback((ev: UltimateEvent) => {
    if (!mounted.current) return;
    switch (ev.type) {
      case "plan":
        setSubtasks(
          ev.subtasks.map((s: UltimateSubtask) => ({
            id: s.id,
            task: s.task,
            kind: s.kind,
            difficulty: s.difficulty,
            fanOut: s.fan_out,
            models: [],
            results: [],
            merged: null,
          })),
        );
        break;
      case "subtask_started":
        setSubtasks((prev) => {
          const exists = prev.some((s) => s.id === ev.id);
          const next: SubtaskState = {
            id: ev.id,
            task: ev.task,
            models: ev.models,
            results: [],
            merged: null,
          };
          return exists
            ? prev.map((s) => (s.id === ev.id ? { ...s, task: ev.task, models: ev.models } : s))
            : [...prev, next];
        });
        break;
      case "model_done":
        setSubtasks((prev) =>
          prev.map((s) =>
            s.id === ev.subtask_id
              ? {
                  ...s,
                  results: [
                    ...s.results.filter((r) => r.model !== ev.model),
                    { model: ev.model, ok: ev.ok, output: ev.output },
                  ],
                }
              : s,
          ),
        );
        break;
      case "subtask_merged":
        setSubtasks((prev) =>
          prev.map((s) => (s.id === ev.id ? { ...s, merged: ev.merged } : s)),
        );
        break;
      case "synthesis":
        setSynthesis(ev.merged);
        break;
      case "cost":
        setCostUsd(ev.usd);
        break;
      case "error":
        setError(ev.msg);
        setRunning(false);
        break;
      case "done":
        setRunning(false);
        break;
    }
  }, []);

  async function run() {
    if (!goal.trim()) {
      setError("Describe a goal for the Ultimate agent to work on.");
      return;
    }
    // Reset the timeline for a fresh run.
    setError(null);
    setSubtasks([]);
    setSynthesis(null);
    setCostUsd(null);
    setRunning(true);

    try {
      localStorage.setItem("cortex.ultimate.projectRoot", projectRoot.trim());
      localStorage.setItem("cortex.ultimate.fanOut", String(fanOut));
    } catch {
      /* private mode / quota — non-fatal */
    }

    let unlisten: UnlistenFn | null = null;
    try {
      unlisten = await subscribeUltimate(handleEvent);
      const result = await ultimateRun({
        goal: goal.trim(),
        projectRoot: projectRoot.trim() || null,
        fanOut,
      });
      if (mounted.current) {
        // The resolved result is the source of truth — backfill anything the
        // event stream didn't paint (e.g. if we attached late).
        setSynthesis((prev) => prev ?? result.final_output);
        setCostUsd((prev) => prev ?? result.total_usd);
      }
    } catch (e) {
      if (mounted.current) setError(humanizeError(e));
    } finally {
      if (unlisten) unlisten();
      if (mounted.current) setRunning(false);
    }
  }

  return (
    <div className="ult-pane">
      <div className="ult-head">
        <h2 className="ult-title">Ultimate agent</h2>
        <p className="ult-subtitle">
          Combines all your connected models: a lead model plans the goal, several
          models race on each subtask, the winners are merged, and a final answer
          is synthesized.
        </p>
      </div>

      {/* Model roster — the "combines all your models" proof. */}
      <div className="ult-roster">
        <div className="ult-label ult-label-row">
          Connected models{models.length > 0 ? ` (${models.length})` : ""}
        </div>
        <div className="ult-chip-row">
          {modelsLoading && models.length === 0 && (
            <span className="ult-hint">Loading models…</span>
          )}
          {!modelsLoading && models.length === 0 && (
            <span className="ult-hint">
              No models connected — connect providers in Settings to fan out.
            </span>
          )}
          {models.map((m) => (
            <span key={m} className="ult-chip" title={m}>
              {m}
            </span>
          ))}
        </div>
      </div>

      <label className="ult-label">
        Goal
        <textarea
          className="ult-input ult-textarea"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder="Describe the goal — the Ultimate agent plans it into subtasks and races your models on each."
          rows={4}
          disabled={running}
        />
      </label>

      <div className="ult-options">
        <label className="ult-label ult-label-grow">
          Project root <span className="muted">(optional)</span>
          <input
            className="ult-input"
            type="text"
            value={projectRoot}
            onChange={(e) => setProjectRoot(e.target.value)}
            placeholder="/path/to/project"
            spellCheck={false}
            disabled={running}
          />
        </label>
        <label className="ult-label ult-label-fanout">
          Fan-out
          <input
            className="ult-input ult-fanout-input"
            type="number"
            min={1}
            max={8}
            step={1}
            value={fanOut}
            onChange={(e) => {
              const n = Number(e.target.value);
              setFanOut(Number.isFinite(n) && n >= 1 ? Math.min(8, Math.round(n)) : 1);
            }}
            disabled={running}
          />
        </label>
      </div>

      <div className="ult-actions">
        <button onClick={() => void run()} disabled={running} className="btn-primary">
          {running ? "Running…" : "Run"}
        </button>
        {costUsd != null && (
          <span
            className="ult-cost-badge"
            title="Estimated total spend across this run (token estimate × model price)"
          >
            ${costUsd.toFixed(4)}
          </span>
        )}
        {error && <span className="ult-error">{error}</span>}
      </div>

      {/* Live timeline. */}
      <div className="ult-timeline">
        {subtasks.length === 0 && !synthesis && !running && (
          <p className="ult-hint">
            No run yet. Describe a goal above and hit <em>Run</em> — the plan, the
            models racing on each subtask, and the final synthesis will stream in
            here.
          </p>
        )}

        {subtasks.length > 0 && (
          <div className="ult-section">
            <div className="ult-label ult-label-row">Plan</div>
            {subtasks.map((s, i) => (
              <SubtaskCard key={s.id} index={i} subtask={s} />
            ))}
          </div>
        )}

        {synthesis != null && (
          <div className="ult-section ult-synthesis">
            <div className="ult-label ult-label-row">Synthesis</div>
            <div className="ult-synthesis-body">
              <MarkdownView source={synthesis} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function SubtaskCard({ index, subtask }: { index: number; subtask: SubtaskState }) {
  return (
    <div className="ult-subtask-card">
      <div className="ult-subtask-head">
        <span className="ult-subtask-num">{index + 1}</span>
        <span className="ult-subtask-task" title={subtask.task}>
          {subtask.task}
        </span>
        {subtask.kind && (
          <span className="ult-tag" title="Task kind from the plan">
            {subtask.kind}
          </span>
        )}
        {subtask.difficulty && (
          <span className="ult-tag" title="Task difficulty from the plan">
            {subtask.difficulty}
          </span>
        )}
        {subtask.fanOut && (
          <span className="ult-tag ult-tag-fanout" title="This subtask fans out across multiple models">
            fan-out
          </span>
        )}
      </div>

      {subtask.models.length > 0 && (
        <div className="ult-subtask-models">
          {subtask.models.map((m) => {
            const result = subtask.results.find((r) => r.model === m);
            const status = result ? (result.ok ? "ok" : "fail") : "running";
            return (
              <span key={m} className={`ult-model-pill ult-model-${status}`} title={m}>
                {m}
                <span className="ult-model-status">
                  {status === "running" ? "…" : status === "ok" ? "✓" : "✗"}
                </span>
              </span>
            );
          })}
        </div>
      )}

      {subtask.results.length > 0 && (
        <div className="ult-model-outputs">
          {subtask.results.map((r) => (
            <details key={r.model} className="ult-model-output">
              <summary className="ult-model-output-summary">
                <span className={`ult-model-dot ult-model-${r.ok ? "ok" : "fail"}`} aria-hidden="true" />
                <span className="ult-model-output-name">{r.model}</span>
                <span className="ult-model-output-state">{r.ok ? "ok" : "failed"}</span>
              </summary>
              <div className="ult-model-output-body">
                <MarkdownView source={r.output || "_(empty)_"} />
              </div>
            </details>
          ))}
        </div>
      )}

      {subtask.merged != null && (
        <div className="ult-subtask-merged">
          <div className="ult-merged-label">Merged</div>
          <div className="ult-merged-body">
            <MarkdownView source={subtask.merged} />
          </div>
        </div>
      )}
    </div>
  );
}
