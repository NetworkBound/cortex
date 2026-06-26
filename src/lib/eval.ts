// Agent eval / benchmark harness — frontend bindings.
//
// Mirrors `src-tauri/src/commands/eval_harness.rs`. Runs the model against a
// fixed task set, scores each against a substring rubric, and persists a
// report. Progress streams over `eval:progress`.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface EvalTask {
  id: string;
  prompt: string;
  expect_contains: string[];
}

export interface EvalResult {
  id: string;
  prompt: string;
  answer: string;
  passed: boolean;
  score: number;
  matched: string[];
  missed: string[];
  latency_ms: number;
  error: string | null;
}

export interface EvalReport {
  run_id: string;
  model: string;
  started_unix_ms: number;
  finished_unix_ms: number;
  total: number;
  passed: number;
  score_avg: number;
  results: EvalResult[];
}

export interface EvalProgress {
  done: number;
  total: number;
  id: string;
  passed: boolean;
  /** Display model for the run (requested slug or the gateway default). */
  model?: string;
}

export async function listEvalTasks(): Promise<EvalTask[]> {
  return invoke<EvalTask[]>("list_eval_tasks");
}

export async function listEvalReports(): Promise<EvalReport[]> {
  return invoke<EvalReport[]>("list_eval_reports");
}

/**
 * Run the benchmark. `model` is any slug the composer picker offers
 * (`claude-…`, `gpt-…`, `ollama:tag`) routed through the adapter registry;
 * omit it to use the default route. `persist: false` keeps the run out of the
 * on-disk history (used by the E2E probe so test runs never pollute real
 * history).
 */
export async function runEval(
  tasks?: EvalTask[],
  opts?: { persist?: boolean; model?: string },
): Promise<EvalReport> {
  return invoke<EvalReport>("run_eval", {
    tasks: tasks ?? null,
    persist: opts?.persist ?? null,
    model: opts?.model ?? null,
  });
}

/**
 * Snapshot of the eval run currently in flight. Queried by the job store on
 * boot so a webview reload mid-run re-adopts the work instead of orphaning it.
 */
export async function evalActive(): Promise<EvalProgress | null> {
  return invoke<EvalProgress | null>("eval_active");
}

export async function onEvalProgress(cb: (p: EvalProgress) => void): Promise<UnlistenFn> {
  return listen<EvalProgress>("eval:progress", (e) => cb(e.payload));
}
