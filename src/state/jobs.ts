// Global job store for long-running work: Cookbook pulls, Deep Research runs
// and Eval runs.
//
// THE PROBLEM THIS SOLVES: ActivityPanel unmounts a tab the moment you switch
// away, and job progress used to live in panel-local useState — switch tabs
// during a multi-GB pull (or mid-research, mid-benchmark) and the progress
// bar, the awaited promise and the completion toast all vanished (and
// re-entering the tab offered a second start of work already running
// backend-side).
//
// THE SHAPE OF THE FIX: the invoke() + event subscription for a job live HERE,
// in module scope, so they survive any unmount. Panels merely *render* the
// store. On completion/failure the outcome goes to a toast AND the
// NotificationCenter inbox (`recordJobEvent`) regardless of which tab is open,
// and the StatusBar shows a live pill while anything is running. A webview
// reload mid-job re-adopts backend-side work via the per-kind in-flight query
// command (`cookbook_active_pulls` / `deep_research_active` / `eval_active`).
//
// Beyond the generic `jobs` record (which drives the StatusBar pill and the
// inbox), Research and Eval keep a kind-specific slice holding their progress
// AND their last result, because their panels render rich output (the report)
// — not just a progress bar. Pulls need no slice: their output is the model
// itself, surfaced via `models:changed`.

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { pushToast } from "@/lib/toast";
import { humanizeError } from "@/lib/errors";
import { recordJobEvent } from "@/lib/notification-center";
import { onPullProgress, pullModel, type PullProgress } from "@/lib/cookbook";
import {
  deepResearch,
  deepResearchActive,
  onResearchProgress,
  type ResearchProgress,
} from "@/lib/deep-research";
import {
  evalActive,
  onEvalProgress,
  runEval,
  type EvalProgress,
  type EvalReport,
  type EvalTask,
} from "@/lib/eval";

export type JobKind = "pull" | "research" | "eval";

export interface Job {
  /** Stable id, `<kind>:<subject>` — e.g. `pull:llama3.2:1b`. */
  id: string;
  kind: JobKind;
  /** Human label for the StatusBar pill / notifications: "Pulling llama3.2:1b". */
  label: string;
  /** Current step, e.g. Ollama's status line ("downloading", "verifying sha256"). */
  detail: string;
  /** 0–100, or null while the total is unknown (indeterminate). */
  pct: number | null;
  startedAt: number;
}

/** The activity tab that owns each job kind — StatusBar pill + inbox deep-links. */
export const JOB_TAB: Record<JobKind, "cookbook" | "research" | "eval"> = {
  pull: "cookbook",
  research: "research",
  eval: "eval",
};

/** A finished research report as the ResearchPanel renders it. */
export interface ResearchView {
  markdown: string;
  /** Vault path of the saved report — null when the vault save failed. */
  path: string | null;
  title: string;
}

/**
 * Research job slice. `progress` is non-null exactly while a run is in
 * flight; `report` survives until the next run (or `clearResearchReport`),
 * so switching tabs mid-run AND after completion loses nothing.
 */
export interface ResearchState {
  question: string | null;
  progress: ResearchProgress | null;
  report: ResearchView | null;
  error: string | null;
}

/** Eval job slice — same contract as `ResearchState`. */
export interface EvalRunState {
  progress: EvalProgress | null;
  report: EvalReport | null;
  error: string | null;
}

interface JobsState {
  /** Running jobs only — settled jobs leave the store (outcome goes to the inbox). */
  jobs: Record<string, Job>;
  research: ResearchState;
  evalRun: EvalRunState;
  begin: (job: Job) => void;
  update: (id: string, patch: Partial<Pick<Job, "detail" | "pct">>) => void;
  end: (id: string) => void;
  patchResearch: (patch: Partial<ResearchState>) => void;
  patchEvalRun: (patch: Partial<EvalRunState>) => void;
}

export const useJobs = create<JobsState>((set) => ({
  jobs: {},
  research: { question: null, progress: null, report: null, error: null },
  evalRun: { progress: null, report: null, error: null },
  begin: (job) => set((s) => ({ jobs: { ...s.jobs, [job.id]: job } })),
  update: (id, patch) =>
    set((s) => {
      const cur = s.jobs[id];
      if (!cur) return s;
      return { jobs: { ...s.jobs, [id]: { ...cur, ...patch } } };
    }),
  end: (id) =>
    set((s) => {
      if (!s.jobs[id]) return s;
      const next = { ...s.jobs };
      delete next[id];
      return { jobs: next };
    }),
  patchResearch: (patch) => set((s) => ({ research: { ...s.research, ...patch } })),
  patchEvalRun: (patch) => set((s) => ({ evalRun: { ...s.evalRun, ...patch } })),
}));

/** Clear the last research report (the panel's "back to saved reports"). */
export function clearResearchReport(): void {
  useJobs.getState().patchResearch({ report: null, error: null });
}

function progressPatch(p: PullProgress): { detail: string; pct: number | null } {
  return { detail: p.status, pct: p.total > 0 ? p.pct : null };
}

/**
 * Start a Cookbook model pull as a global job. Owns the whole lifecycle in
 * module scope; safe to fire-and-forget from any component. A second call for
 * a model already pulling is a no-op (the backend enforces the same guard).
 */
export async function startCookbookPull(name: string): Promise<void> {
  const id = `pull:${name}`;
  const { jobs, begin, update, end } = useJobs.getState();
  if (jobs[id]) {
    pushToast({ title: `'${name}' is already being pulled`, kind: "info" });
    return;
  }
  begin({ id, kind: "pull", label: `Pulling ${name}`, detail: "starting", pct: null, startedAt: Date.now() });
  let unlisten: (() => void) | null = null;
  try {
    unlisten = await onPullProgress(name, (p) => update(id, progressPatch(p)));
    const res = await pullModel(name);
    pushToast({ title: res.message, kind: "success" });
    recordJobEvent({ kind: "pull", label: `Pull ${name}`, ok: true, detail: res.message });
  } catch (e) {
    const msg = humanizeError(e);
    pushToast({ title: msg, kind: "error" });
    recordJobEvent({ kind: "pull", label: `Pull ${name}`, ok: false, detail: msg });
  } finally {
    unlisten?.();
    end(id);
  }
}

/** One research run at a time (the backend enforces the same guard). */
const RESEARCH_JOB_ID = "research:run";

function researchJobPatch(p: ResearchProgress): { detail: string; pct: number } {
  return { detail: p.message ? `${p.step} — ${p.message}` : p.step, pct: p.pct };
}

/**
 * Start a Deep Research run as a global job. Owns the whole lifecycle in
 * module scope — `/research <q>` fires this without the panel mounted, and a
 * tab switch mid-run loses neither the progress nor the finished report
 * (it lands in `research.report` + the vault, with a toast + inbox entry).
 */
export async function startDeepResearch(question: string, maxSources = 5): Promise<void> {
  const q = question.trim();
  if (!q) return;
  const { jobs, begin, update, end, patchResearch } = useJobs.getState();
  if (jobs[RESEARCH_JOB_ID]) {
    pushToast({ title: "A research run is already in progress", kind: "info" });
    return;
  }
  const label = `Researching: ${q.length > 48 ? `${q.slice(0, 48)}…` : q}`;
  begin({ id: RESEARCH_JOB_ID, kind: "research", label, detail: "starting", pct: 0, startedAt: Date.now() });
  patchResearch({
    question: q,
    progress: { step: "starting", status: "start", message: null, pct: 0 },
    report: null,
    error: null,
  });
  let unlisten: (() => void) | null = null;
  try {
    unlisten = await onResearchProgress((p) => {
      update(RESEARCH_JOB_ID, researchJobPatch(p));
      useJobs.getState().patchResearch({ progress: p });
    });
    const r = await deepResearch(q, maxSources);
    useJobs.getState().patchResearch({
      report: { markdown: r.markdown, path: r.saved_path, title: r.question || q },
    });
    pushToast({ title: "Research report ready", body: r.question || q, kind: "success" });
    recordJobEvent({ kind: "research", label, ok: true, detail: r.saved_path ?? "report ready (vault save failed)" });
  } catch (e) {
    const msg = humanizeError(e);
    useJobs.getState().patchResearch({ error: msg });
    pushToast({ title: "Research failed", body: msg, kind: "error" });
    recordJobEvent({ kind: "research", label, ok: false, detail: msg });
  } finally {
    unlisten?.();
    useJobs.getState().patchResearch({ progress: null });
    end(RESEARCH_JOB_ID);
  }
}

/** One eval run at a time (the backend enforces the same guard). */
const EVAL_JOB_ID = "eval:run";

function evalJobPatch(p: EvalProgress): { detail: string; pct: number | null } {
  const step = p.id ? `${p.done}/${p.total} · ${p.id}` : `${p.done}/${p.total} tasks`;
  return {
    detail: p.model ? `${step} · ${p.model}` : step,
    pct: p.total > 0 ? Math.round((p.done / p.total) * 100) : null,
  };
}

/**
 * Start a benchmark run as a global job — same module-scope ownership as
 * `startDeepResearch`. `model` is a composer-picker slug routed through the
 * adapter registry (omit for the default route). `tasks`/`persist` exist for
 * the E2E probe (a tiny task set, `persist: false`); the EvalPanel passes
 * only `model`.
 */
export async function startEvalRun(
  opts?: { tasks?: EvalTask[]; persist?: boolean; model?: string },
): Promise<void> {
  const { jobs, begin, update, end, patchEvalRun } = useJobs.getState();
  if (jobs[EVAL_JOB_ID]) {
    pushToast({ title: "A benchmark run is already in progress", kind: "info" });
    return;
  }
  begin({
    id: EVAL_JOB_ID,
    kind: "eval",
    label: "Running benchmark",
    detail: opts?.model ? `starting · ${opts.model}` : "starting",
    pct: 0,
    startedAt: Date.now(),
  });
  patchEvalRun({
    progress: { done: 0, total: opts?.tasks?.length ?? 0, id: "", passed: false, model: opts?.model },
    error: null,
  });
  let unlisten: (() => void) | null = null;
  try {
    unlisten = await onEvalProgress((p) => {
      update(EVAL_JOB_ID, evalJobPatch(p));
      useJobs.getState().patchEvalRun({ progress: p });
    });
    const r = await runEval(opts?.tasks, { persist: opts?.persist, model: opts?.model });
    useJobs.getState().patchEvalRun({ report: r });
    pushToast({
      title: `Benchmark done — ${r.passed}/${r.total} passed`,
      body: r.model,
      kind: r.passed === r.total ? "success" : "warning",
    });
    recordJobEvent({
      kind: "eval",
      label: "Benchmark run",
      ok: true,
      detail: `${r.passed}/${r.total} passed (${Math.round(r.score_avg * 100)}% avg) on ${r.model}`,
    });
  } catch (e) {
    const msg = humanizeError(e);
    useJobs.getState().patchEvalRun({ error: msg });
    pushToast({ title: "Benchmark failed", body: msg, kind: "error" });
    recordJobEvent({ kind: "eval", label: "Benchmark run", ok: false, detail: msg });
  } finally {
    unlisten?.();
    useJobs.getState().patchEvalRun({ progress: null });
    end(EVAL_JOB_ID);
  }
}

// ---------------------------------------------------------------------------
// Reload adoption — re-attach to backend-side work this webview didn't start
// ---------------------------------------------------------------------------

const ADOPT_POLL_MS = 3000;

/**
 * Adopt pulls that are still streaming backend-side but unknown to this
 * webview — i.e. after a reload mid-pull. Progress keeps flowing from the
 * still-live `cookbook:pull:<name>` events; completion is detected by the
 * name leaving `cookbook_active_pulls` (the original invoke() belonged to the
 * previous webview, so its resolution is unobservable here). The outcome
 * detail is the last status Ollama reported — honest, if less precise than a
 * first-hand result.
 */
async function adoptInFlightPulls(): Promise<void> {
  const active = await invoke<PullProgress[]>("cookbook_active_pulls");
  for (const p of active) {
    const id = `pull:${p.name}`;
    const { jobs, begin, update, end } = useJobs.getState();
    if (jobs[id]) continue;
    begin({
      id,
      kind: "pull",
      label: `Pulling ${p.name}`,
      ...progressPatch(p),
      startedAt: Date.now(),
    });
    let unlisten: () => void;
    try {
      unlisten = await onPullProgress(p.name, (prog) => update(id, progressPatch(prog)));
    } catch {
      end(id); // can't subscribe — don't leave a phantom job behind
      continue;
    }
    const watch = window.setInterval(() => {
      void invoke<PullProgress[]>("cookbook_active_pulls")
        .then((rows) => {
          if (rows.some((r) => r.name === p.name)) return;
          window.clearInterval(watch);
          unlisten();
          const last = useJobs.getState().jobs[id]?.detail ?? "finished";
          end(id);
          const ok = last.toLowerCase().includes("success");
          recordJobEvent({
            kind: "pull",
            label: `Pull ${p.name}`,
            ok,
            detail: `settled after reload (last status: ${last})`,
          });
          pushToast({
            title: ok ? `Pulled '${p.name}'` : `Pull of '${p.name}' ended (${last})`,
            kind: ok ? "success" : "warning",
          });
        })
        .catch(() => {/* backend hiccup — keep watching */});
    }, ADOPT_POLL_MS);
  }
}

/**
 * Adopt a research run still in flight backend-side after a reload. Same
 * contract as `adoptInFlightPulls`: progress keeps flowing from the live
 * `deep_research:progress` events; completion is detected by the registry
 * emptying. The finished report itself resolved into the previous webview —
 * but it's saved to the vault, so the outcome points at Saved reports.
 */
async function adoptInFlightResearch(): Promise<void> {
  const active = await deepResearchActive();
  if (!active) return;
  const { jobs, begin, update, patchResearch } = useJobs.getState();
  if (jobs[RESEARCH_JOB_ID]) return;
  const q = active.question;
  const label = `Researching: ${q.length > 48 ? `${q.slice(0, 48)}…` : q}`;
  begin({
    id: RESEARCH_JOB_ID,
    kind: "research",
    label,
    ...researchJobPatch(active.progress),
    startedAt: Date.now(),
  });
  patchResearch({ question: q, progress: active.progress, report: null, error: null });
  let unlisten: () => void;
  try {
    unlisten = await onResearchProgress((p) => {
      update(RESEARCH_JOB_ID, researchJobPatch(p));
      useJobs.getState().patchResearch({ progress: p });
    });
  } catch {
    useJobs.getState().end(RESEARCH_JOB_ID);
    patchResearch({ progress: null });
    return;
  }
  const watch = window.setInterval(() => {
    void deepResearchActive()
      .then((row) => {
        if (row) return;
        window.clearInterval(watch);
        unlisten();
        const { end: endJob, patchResearch: patch } = useJobs.getState();
        endJob(RESEARCH_JOB_ID);
        patch({ progress: null });
        recordJobEvent({
          kind: "research",
          label,
          ok: true,
          detail: "settled after reload — open it from Saved reports",
        });
        pushToast({ title: "Research run finished", body: "Open it from Saved reports", kind: "success" });
      })
      .catch(() => {/* backend hiccup — keep watching */});
  }, ADOPT_POLL_MS);
}

/**
 * Adopt an eval run still in flight backend-side after a reload. Completion
 * is detected by the registry emptying; the report is persisted to history
 * backend-side, so the outcome points at Past runs.
 */
async function adoptInFlightEval(): Promise<void> {
  const active = await evalActive();
  if (!active) return;
  const { jobs, begin, update, patchEvalRun } = useJobs.getState();
  if (jobs[EVAL_JOB_ID]) return;
  begin({
    id: EVAL_JOB_ID,
    kind: "eval",
    label: "Running benchmark",
    ...evalJobPatch(active),
    startedAt: Date.now(),
  });
  patchEvalRun({ progress: active, error: null });
  let unlisten: () => void;
  try {
    unlisten = await onEvalProgress((p) => {
      update(EVAL_JOB_ID, evalJobPatch(p));
      useJobs.getState().patchEvalRun({ progress: p });
    });
  } catch {
    useJobs.getState().end(EVAL_JOB_ID);
    patchEvalRun({ progress: null });
    return;
  }
  const watch = window.setInterval(() => {
    void evalActive()
      .then((row) => {
        if (row) return;
        window.clearInterval(watch);
        unlisten();
        const { end: endJob, patchEvalRun: patch } = useJobs.getState();
        endJob(EVAL_JOB_ID);
        patch({ progress: null });
        recordJobEvent({
          kind: "eval",
          label: "Benchmark run",
          ok: true,
          detail: "settled after reload — see Past runs for the report",
        });
        pushToast({ title: "Benchmark run finished", body: "See Past runs for the report", kind: "success" });
      })
      .catch(() => {/* backend hiccup — keep watching */});
  }, ADOPT_POLL_MS);
}

let initialized = false;

/**
 * One-time boot hookup (called from the StatusBar, which is always mounted):
 * re-adopt any backend-side in-flight work so a reload never orphans a job.
 */
export function initJobStore(): void {
  if (initialized) return;
  initialized = true;
  void adoptInFlightPulls().catch(() => {/* backend not ready — jobs start fresh */});
  void adoptInFlightResearch().catch(() => {/* backend not ready */});
  void adoptInFlightEval().catch(() => {/* backend not ready */});
}
