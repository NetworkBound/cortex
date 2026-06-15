/**
 * Workflows panel — card list of preset multi-step recipes. Each card shows
 * name + description + step count, with one-click Run / Edit / Delete.
 *
 * Workflows live as YAML at `~/.cortex/workflows/<name>.yaml`; the backend
 * seeds five sensible defaults on first launch (review-pr, morning-standup,
 * triage-bug, prep-release, audit-deps).
 *
 * "Run" calls `run_workflow` on the backend to expand the YAML into an
 * ordered step list, then appends each `[role:<name>] <prompt>` as a system
 * note in the chat. The existing chat pipeline picks each one up as the
 * user follows along — v1 is intentionally NOT auto-dispatching to keep us
 * off the streaming critical path.
 *
 * The editor is a textarea-driven YAML-ish form (one row per step) so power
 * users can twiddle steps without leaving the panel. Dirty-tracking is
 * shallow: any field change marks the draft dirty until Save succeeds.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  deleteWorkflow,
  formatStepPrompt,
  listWorkflows,
  runWorkflow,
  saveWorkflow,
  type Workflow,
  type WorkflowStep,
} from "@/lib/workflows";
import { useCortexStore, type Message } from "@/state/store";
import { PanelLoading } from "./Skeleton";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";

const DRAFT_ID = "__draft__";

interface Draft {
  origName: string | null;
  name: string;
  description: string;
  steps: WorkflowStep[];
}

function systemNote(content: string): Message {
  return { id: `wf-${crypto.randomUUID()}`, role: "system", content, tools: [] };
}

function emptyDraft(): Draft {
  return {
    origName: null,
    name: "",
    description: "",
    steps: [{ role: "code-reviewer", prompt: "" }],
  };
}

function nameValid(name: string): boolean {
  const n = name.trim();
  if (!n || n.length > 64) return false;
  return /^[A-Za-z0-9_\-.]+$/.test(n);
}

export function WorkflowsPanel() {
  const [workflows, setWorkflows] = useState<Workflow[] | null>(null);
  const [activeName, setActiveName] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [saving, setSaving] = useState(false);
  const [busyRun, setBusyRun] = useState<string | null>(null);

  const reload = useCallback(async () => {
    const list = await listWorkflows();
    setWorkflows(list);
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Sync the editor draft to whichever workflow is currently selected.
  useEffect(() => {
    if (activeName === DRAFT_ID) return;
    if (!workflows || activeName === null) {
      setDraft(null);
      return;
    }
    const wf = workflows.find((x) => x.name === activeName);
    if (!wf) {
      setDraft(null);
      return;
    }
    setDraft({
      origName: wf.name,
      name: wf.name,
      description: wf.description ?? "",
      steps: wf.steps.length > 0
        ? wf.steps.map((s) => ({ ...s }))
        : [{ role: "code-reviewer", prompt: "" }],
    });
  }, [activeName, workflows]);

  const dirty = useMemo(() => {
    if (!draft) return false;
    if (draft.origName === null) return true;
    const orig = workflows?.find((s) => s.name === draft.origName);
    if (!orig) return true;
    if (draft.name !== orig.name) return true;
    if ((draft.description ?? "") !== (orig.description ?? "")) return true;
    if (draft.steps.length !== orig.steps.length) return true;
    return draft.steps.some((s, i) => {
      const o = orig.steps[i];
      return !o || s.role !== o.role || s.prompt !== o.prompt;
    });
  }, [draft, workflows]);

  function startNew() {
    setActiveName(DRAFT_ID);
    setDraft(emptyDraft());
  }

  async function handleSave() {
    if (!draft) return;
    if (!nameValid(draft.name)) {
      pushToast({
        title: "Invalid name",
        body: "Letters, numbers, _ - . only.",
        kind: "warning",
      });
      return;
    }
    const cleanedSteps = draft.steps
      .map((s) => ({ role: s.role.trim(), prompt: s.prompt.trim() }))
      .filter((s) => s.role && s.prompt);
    if (cleanedSteps.length === 0) {
      pushToast({ title: "Save skipped", body: "Add at least one step.", kind: "warning" });
      return;
    }
    setSaving(true);
    try {
      const saved = await saveWorkflow({
        name: draft.name.trim(),
        description: draft.description.trim() || null,
        steps: cleanedSteps,
      });
      if (!saved) {
        pushToast({ title: "Save failed", body: "Backend rejected workflow.", kind: "error" });
        return;
      }
      if (draft.origName && draft.origName !== saved.name) {
        await deleteWorkflow(draft.origName);
      }
      pushToast({ title: "Saved", body: saved.name, kind: "success" });
      setActiveName(saved.name);
      await reload();
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(name: string) {
    if (!(await confirmDialog({
      title: "Delete workflow?",
      message: `Delete workflow "${name}"? This cannot be undone.`,
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    const ok = await deleteWorkflow(name);
    if (!ok) {
      pushToast({ title: "Delete failed", body: name, kind: "error" });
      return;
    }
    pushToast({ title: "Deleted", body: name, kind: "info" });
    if (activeName === name) setActiveName(null);
    await reload();
  }

  /**
   * Launch a workflow. We resolve the steps backend-side (single roundtrip,
   * proper path-safety enforcement) then append one system note per step so
   * the user can read the queue, copy a prompt into the composer, and let
   * the existing chat pipeline take it from there.
   */
  async function handleRun(name: string) {
    setBusyRun(name);
    try {
      const run = await runWorkflow(name);
      if (!run) {
        pushToast({ title: "Run failed", body: name, kind: "error" });
        return;
      }
      const append = useCortexStore.getState().appendMessage;
      append(
        systemNote(
          `▶︎ workflow **${run.name}** queued — ${run.steps.length} step${
            run.steps.length === 1 ? "" : "s"
          } (\`${run.run_id}\`)`,
        ),
      );
      run.steps.forEach((step, idx) => {
        append(
          systemNote(
            `**Step ${idx + 1}/${run.steps.length}** · ${formatStepPrompt(step)}`,
          ),
        );
      });
      pushToast({
        title: "Workflow queued",
        body: `${run.name} · ${run.steps.length} step${run.steps.length === 1 ? "" : "s"}`,
        kind: "success",
      });
    } finally {
      setBusyRun(null);
    }
  }

  function updateStep(i: number, patch: Partial<WorkflowStep>) {
    setDraft((d) => {
      if (!d) return d;
      const next = d.steps.slice();
      next[i] = { ...next[i], ...patch };
      return { ...d, steps: next };
    });
  }

  function addStep() {
    setDraft((d) =>
      d ? { ...d, steps: [...d.steps, { role: "", prompt: "" }] } : d,
    );
  }

  function removeStep(i: number) {
    setDraft((d) => {
      if (!d) return d;
      const next = d.steps.slice();
      next.splice(i, 1);
      return {
        ...d,
        steps: next.length === 0 ? [{ role: "", prompt: "" }] : next,
      };
    });
  }

  if (workflows === null) {
    return <PanelLoading label="Loading workflows" />;
  }

  const showingDraft = activeName === DRAFT_ID;
  const hasAny = workflows.length > 0 || showingDraft;

  return (
    <div className="workflows-panel">
      <div className="workflows-list">
        <div className="workflows-list-head">
          <span className="muted">
            {workflows.length} workflow{workflows.length === 1 ? "" : "s"}
          </span>
          <div className="workflows-list-head-actions">
            <button type="button" className="panel-head-action" onClick={startNew}>
              + new
            </button>
            <button type="button" className="panel-head-action ghost" onClick={() => void reload()}>
              Refresh
            </button>
          </div>
        </div>
        {showingDraft && (
          <div className="workflows-card active">
            <div className="workflows-card-title">
              {draft?.name.trim() || <span className="muted">(unsaved)</span>}
            </div>
            <div className="workflows-card-desc muted">new — unsaved</div>
          </div>
        )}
        {workflows.length === 0 && !showingDraft && (
          <div className="muted workflows-empty">
            No workflows yet. Hit + new to build one.
          </div>
        )}
        {workflows.map((wf) => (
          <div
            key={wf.name}
            className={`workflows-card ${activeName === wf.name ? "active" : ""}`}
          >
            <button
              type="button"
              className="workflows-card-main"
              onClick={() => setActiveName(wf.name)}
            >
              <div className="workflows-card-title">{wf.name}</div>
              {wf.description && (
                <div className="workflows-card-desc">{wf.description}</div>
              )}
              <div className="workflows-card-meta muted">
                {wf.steps.length} step{wf.steps.length === 1 ? "" : "s"}
              </div>
            </button>
            <div className="workflows-card-actions">
              <button
                type="button"
                className="btn-primary"
                disabled={busyRun === wf.name}
                onClick={() => void handleRun(wf.name)}
              >
                {busyRun === wf.name ? "…" : "Run"}
              </button>
              <button
                type="button"
                className="link-btn"
                onClick={() => setActiveName(wf.name)}
              >
                Edit
              </button>
              <button
                type="button"
                className="link-btn danger"
                onClick={() => void handleDelete(wf.name)}
              >
                ×
              </button>
            </div>
          </div>
        ))}
      </div>
      <div className="workflows-detail">
        {!hasAny && (
          <div className="muted workflows-empty">
            No workflows yet. Build one to automate a multi-step task.
            <div className="skills-list-head-actions" style={{ marginTop: "var(--space-3)", justifyContent: "center" }}>
              <button type="button" className="panel-head-action" onClick={startNew}>
                + new workflow
              </button>
            </div>
          </div>
        )}
        {hasAny && !draft && (
          <div className="muted workflows-empty">
            Select a workflow on the left, or create a new one.
          </div>
        )}
        {draft && (
          <div className="workflows-editor">
            <label className="workflows-field">
              <span className="workflows-label">name</span>
              <input
                type="text"
                value={draft.name}
                placeholder="e.g. review-pr"
                onChange={(e) =>
                  setDraft((d) => (d ? { ...d, name: e.target.value } : d))
                }
              />
              {!nameValid(draft.name) && draft.name.length > 0 && (
                <span className="workflows-name-hint">
                  Letters, numbers, <code>_ - .</code> only (max 64 chars).
                </span>
              )}
            </label>
            <label className="workflows-field">
              <span className="workflows-label">description</span>
              <input
                type="text"
                value={draft.description}
                placeholder="What this workflow does, one line."
                onChange={(e) =>
                  setDraft((d) => (d ? { ...d, description: e.target.value } : d))
                }
              />
            </label>
            <div className="workflows-field">
              <span className="workflows-label">steps</span>
              <div className="workflows-steps">
                {draft.steps.map((step, i) => (
                  <div key={i} className="workflows-step">
                    <div className="workflows-step-head">
                      <span className="muted">step {i + 1}</span>
                      <button
                        type="button"
                        className="link-btn danger"
                        onClick={() => removeStep(i)}
                        disabled={draft.steps.length === 1}
                      >
                        Remove
                      </button>
                    </div>
                    <input
                      type="text"
                      value={step.role}
                      placeholder="role (e.g. code-reviewer)"
                      onChange={(e) => updateStep(i, { role: e.target.value })}
                    />
                    <textarea
                      value={step.prompt}
                      rows={3}
                      placeholder="Prompt body for this step…"
                      onChange={(e) => updateStep(i, { prompt: e.target.value })}
                    />
                  </div>
                ))}
                <button
                  type="button"
                  className="link-btn"
                  onClick={addStep}
                >
                  + add step
                </button>
              </div>
            </div>
            <div className="workflows-actions">
              <button
                type="button"
                className="btn-primary"
                disabled={!dirty || saving}
                onClick={() => void handleSave()}
              >
                {saving ? "Saving…" : "Save"}
              </button>
              {dirty && (
                <span className="muted workflows-dirty">unsaved changes</span>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
