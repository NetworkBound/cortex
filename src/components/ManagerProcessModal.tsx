import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";

import {
  managerDecompose,
  managerRunStep,
  managerValidate,
  statusLabel,
  type Plan,
  type Subtask,
  type SubtaskStatus,
  type Validation,
} from "@/lib/manager-process";
import { pushToast } from "@/lib/toast";

/**
 * Manager process modal — `/manager <goal>` (alias `/auto`).
 *
 * Three-phase UX:
 *   1. Goal textarea + "Decompose" button — calls `manager_decompose`.
 *   2. Plan view — numbered subtasks with role badges, dep arrows, per-step
 *      Run / Validate buttons, and a "Run all" button that walks the plan
 *      sequentially (the backend auto-validates between steps).
 *   3. Per-step output drawer — collapsible <details> for each completed step.
 *
 * Self-mounting portal so `/manager` can summon it without touching App.tsx —
 * same detached-root pattern as DocGenModal / RefactorSuggesterModal.
 */

interface ManagerProcessModalProps {
  initialGoal?: string;
  onClose: () => void;
}

type PerStepValidation = Record<number, Validation>;

export function ManagerProcessModal({
  initialGoal,
  onClose,
}: ManagerProcessModalProps) {
  const [goal, setGoal] = useState<string>(initialGoal ?? "");
  const [plan, setPlan] = useState<Plan | null>(null);
  const [decomposing, setDecomposing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [runningIdx, setRunningIdx] = useState<number | null>(null);
  const [validations, setValidations] = useState<PerStepValidation>({});
  const [openOutputs, setOpenOutputs] = useState<Set<number>>(new Set());
  const [runAllActive, setRunAllActive] = useState(false);

  // ESC closes — only when no step is actively running, so we don't strand a
  // backend call. Users can still bail via the X button if they really want.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (runningIdx !== null || runAllActive) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, runningIdx, runAllActive]);

  const handleDecompose = useCallback(async () => {
    const trimmed = goal.trim();
    if (!trimmed) {
      pushToast({ title: "Manager process", body: "Enter a goal first.", kind: "warning" });
      return;
    }
    setDecomposing(true);
    setError(null);
    try {
      const p = await managerDecompose(trimmed);
      setPlan(p);
      setValidations({});
      setOpenOutputs(new Set());
      pushToast({
        title: "Plan ready",
        body: `${p.subtasks.length} subtask${p.subtasks.length === 1 ? "" : "s"}.`,
        kind: "success",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setDecomposing(false);
    }
  }, [goal]);

  const setSubtaskStatus = useCallback(
    (idx: number, status: SubtaskStatus, output?: string) => {
      setPlan((prev) => {
        if (!prev) return prev;
        const subtasks = prev.subtasks.slice();
        const target = subtasks[idx];
        if (!target) return prev;
        subtasks[idx] = {
          ...target,
          status,
          output: output ?? target.output,
        };
        return { ...prev, subtasks };
      });
    },
    [],
  );

  const runStep = useCallback(
    async (idx: number) => {
      if (!plan) return null;
      setRunningIdx(idx);
      setError(null);
      // Optimistic flip — the backend will eventually return the authoritative
      // status, but the pill should change instantly on click.
      setSubtaskStatus(idx, "running");
      try {
        const result = await managerRunStep(plan.plan_id, idx);
        setValidations((v) => ({ ...v, [idx]: result.validation }));
        setSubtaskStatus(
          idx,
          result.validation.ok ? "done" : "failed",
          result.output,
        );
        setOpenOutputs((set) => {
          const next = new Set(set);
          next.add(idx);
          return next;
        });
        return result.validation;
      } catch (e) {
        setError(`Step ${idx + 1} failed: ${humanizeError(e)}`);
        setSubtaskStatus(idx, "failed");
        return null;
      } finally {
        setRunningIdx(null);
      }
    },
    [plan, setSubtaskStatus],
  );

  const validateStep = useCallback(
    async (idx: number) => {
      if (!plan) return;
      const subtask = plan.subtasks[idx];
      if (!subtask?.output) {
        pushToast({
          title: "Nothing to validate",
          body: "Run the step first, or paste an output.",
          kind: "warning",
        });
        return;
      }
      setRunningIdx(idx);
      try {
        const v = await managerValidate(plan.plan_id, idx, subtask.output);
        setValidations((vs) => ({ ...vs, [idx]: v }));
        setSubtaskStatus(idx, v.ok ? "done" : "failed");
      } catch (e) {
        setError(`Validate ${idx + 1} failed: ${humanizeError(e)}`);
      } finally {
        setRunningIdx(null);
      }
    },
    [plan, setSubtaskStatus],
  );

  const runAll = useCallback(async () => {
    if (!plan) return;
    setRunAllActive(true);
    setError(null);
    for (let i = 0; i < plan.subtasks.length; i++) {
      // Skip already-completed steps so re-runs only fill in the rest.
      const current = plan.subtasks[i];
      if (current.status === "done") continue;
      const validation = await runStep(i);
      if (!validation || !validation.ok) {
        pushToast({
          title: "Run all halted",
          body: `Step ${i + 1} failed validation.`,
          kind: "error",
        });
        break;
      }
    }
    setRunAllActive(false);
  }, [plan, runStep]);

  const toggleOutput = useCallback((idx: number) => {
    setOpenOutputs((set) => {
      const next = new Set(set);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  }, []);

  // Auto-focus the textarea when the modal mounts without a pre-filled goal.
  // We use a callback ref so we don't fight with React Strict Mode double-mount.
  const textareaRef = useCallback(
    (el: HTMLTextAreaElement | null) => {
      if (el && !initialGoal) el.focus();
    },
    [initialGoal],
  );

  return (
    <div className="manager-overlay" role="dialog" aria-modal>
      <div className="manager-modal">
        <header className="manager-header">
          <div>
            <div className="manager-title">Manager process</div>
            <div className="manager-subtitle">
              CrewAI-style auto-decomposition. The manager LLM picks specialists, you press Run.
            </div>
          </div>
          <button
            className="manager-close"
            onClick={onClose}
            aria-label="Close"
            disabled={runningIdx !== null || runAllActive}
            title={
              runningIdx !== null || runAllActive
                ? "Wait for the current step to finish"
                : "Close (Esc)"
            }
          >
            ×
          </button>
        </header>

        <section className="manager-goal">
          <label htmlFor="manager-goal-input" className="manager-label">
            Goal
          </label>
          <textarea
            id="manager-goal-input"
            ref={textareaRef}
            className="manager-goal-input"
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            placeholder="e.g. Add a `/snippets` shortcut to the README, update tests, write a changelog entry."
            rows={3}
            disabled={decomposing || runningIdx !== null || runAllActive}
          />
          <div className="manager-actions">
            <button
              className="manager-primary"
              onClick={handleDecompose}
              disabled={
                decomposing ||
                !goal.trim() ||
                runningIdx !== null ||
                runAllActive
              }
            >
              {decomposing ? "Decomposing…" : plan ? "Re-decompose" : "Decompose"}
            </button>
            {plan && (
              <button
                className="manager-secondary"
                onClick={runAll}
                disabled={runningIdx !== null || runAllActive}
              >
                {runAllActive ? "Running all…" : "Run all"}
              </button>
            )}
          </div>
        </section>

        {error && (
          <div className="manager-error" role="alert">
            {error}
          </div>
        )}

        {plan && (
          <ol className="manager-steps">
            {plan.subtasks.map((s, idx) => (
              <StepCard
                key={idx}
                index={idx}
                subtask={s}
                validation={validations[idx]}
                isOpen={openOutputs.has(idx)}
                isBusy={runningIdx === idx}
                disabledExternal={runningIdx !== null || runAllActive}
                onToggle={() => toggleOutput(idx)}
                onRun={() => runStep(idx)}
                onValidate={() => validateStep(idx)}
              />
            ))}
          </ol>
        )}
      </div>
    </div>
  );
}

interface StepCardProps {
  index: number;
  subtask: Subtask;
  validation?: Validation;
  isOpen: boolean;
  isBusy: boolean;
  disabledExternal: boolean;
  onToggle: () => void;
  onRun: () => void;
  onValidate: () => void;
}

function StepCard({
  index,
  subtask,
  validation,
  isOpen,
  isBusy,
  disabledExternal,
  onToggle,
  onRun,
  onValidate,
}: StepCardProps) {
  // Resolve the displayed status: prefer the backend's authoritative value,
  // fall back to a freshly-attached validation result so the failure case is
  // visible even before the next render cycle.
  const status = subtask.status;
  const depsLabel = useMemo(() => {
    if (!subtask.depends_on.length) return null;
    return subtask.depends_on
      .map((d) => `#${d + 1}`)
      .join(", ");
  }, [subtask.depends_on]);

  return (
    <li className={`manager-step manager-step-${status}`}>
      <div className="manager-step-head">
        <div className="manager-step-num">{index + 1}</div>
        <div className="manager-step-meta">
          <div className="manager-step-name">{subtask.name}</div>
          <div className="manager-step-tags">
            <span className="manager-role-badge">{subtask.role}</span>
            {depsLabel && (
              <span className="manager-dep-badge" title="Depends on prior steps">
                ↳ {depsLabel}
              </span>
            )}
            <span className={`manager-status-pill manager-status-${status}`}>
              {statusLabel(status)}
            </span>
          </div>
        </div>
        <div className="manager-step-actions">
          <button
            className="manager-step-btn"
            onClick={onRun}
            disabled={disabledExternal}
            title="Run this step"
          >
            {isBusy && status !== "validating" ? "Running…" : "Run"}
          </button>
          <button
            className="manager-step-btn"
            onClick={onValidate}
            disabled={disabledExternal || !subtask.output}
            title={subtask.output ? "Re-validate the current output" : "No output yet"}
          >
            {isBusy && status === "validating" ? "Validating…" : "Validate"}
          </button>
        </div>
      </div>

      <div className="manager-step-prompt">{subtask.prompt}</div>

      {validation && (
        <div
          className={`manager-validation ${validation.ok ? "ok" : "fail"}`}
          role="status"
        >
          <strong>{validation.ok ? "✓ validated" : "✗ rejected"}:</strong>{" "}
          {validation.reason}
        </div>
      )}

      {subtask.output && (
        <details
          className="manager-output"
          open={isOpen}
          onToggle={(e) => {
            const el = e.currentTarget as HTMLDetailsElement;
            if (el.open !== isOpen) onToggle();
          }}
        >
          <summary>Output</summary>
          <pre className="manager-output-body">{subtask.output}</pre>
        </details>
      )}
    </li>
  );
}

// ── Self-mounting portal ───────────────────────────────────────────────────

let activeRoot: Root | null = null;

/**
 * Imperative summoner used by the `/manager` slash command. Same detached-
 * root pattern as `DocGenModal` so App.tsx stays untouched.
 */
export function openManagerProcessModal(initialGoal?: string): void {
  if (activeRoot) return; // already open

  const container = document.createElement("div");
  container.dataset.cortexMount = "manager-process";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<ManagerProcessModal initialGoal={initialGoal} onClose={close} />);
}
