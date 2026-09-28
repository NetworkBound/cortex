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

// ── Retrieval-quality eval (same Rust module) ───────────────────────────────

/** One fixture query: which sources should appear among the top-k citations. */
export interface RetrievalEvalTask {
  id: string;
  query: string;
  expect_sources: string[];
}

export interface RetrievalEvalResult {
  id: string;
  query: string;
  passed: boolean;
  /** Fraction of `expect_sources` found among the retrieved references. */
  score: number;
  matched: string[];
  missed: string[];
  /** Display references of the retrieved top-k. */
  retrieved: string[];
  /** How many retrieved chunks were stale (source changed since indexing). */
  stale_retrieved: number;
  latency_ms: number;
}

export interface RetrievalEvalReport {
  run_id: string;
  /** Embedding model the index + queries used (retrieval has no chat model). */
  embed_model: string;
  started_unix_ms: number;
  finished_unix_ms: number;
  total: number;
  passed: number;
  score_avg: number;
  /** Top-k depth each query was scored at. */
  k: number;
  results: RetrievalEvalResult[];
}

/** The user's `~/.cortex/retrieval-eval-tasks.json` fixture; `[]` when absent. */
export async function listRetrievalEvalTasks(): Promise<RetrievalEvalTask[]> {
  return invoke<RetrievalEvalTask[]>("list_retrieval_eval_tasks");
}

export async function listRetrievalEvalReports(): Promise<
  RetrievalEvalReport[]
> {
  return invoke<RetrievalEvalReport[]>("list_retrieval_eval_reports");
}

/**
 * Score the retrieval baseline against the fixture (embeds each query with
 * the local Ollama embedder). `k` is clamped to 1–12 by the backend (default
 * 8). Rejects when there is no fixture file.
 */
export async function runRetrievalEval(
  k?: number,
): Promise<RetrievalEvalReport> {
  return invoke<RetrievalEvalReport>("run_retrieval_eval", {
    tasks: null,
    k: k ?? null,
    persist: null,
  });
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

export async function onEvalProgress(
  cb: (p: EvalProgress) => void,
): Promise<UnlistenFn> {
  return listen<EvalProgress>("eval:progress", (e) => cb(e.payload));
}
