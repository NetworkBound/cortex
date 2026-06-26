import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Frontend bridge to the `batch_run` Tauri command.
 *
 * Mirrors `BatchItem` / `BatchRunReport` from
 * `src-tauri/src/commands/batch_runner.rs` one-for-one. Progress is
 * streamed via `batch:progress:<run_id>` window events — callers should
 * subscribe with `listenBatchProgress` BEFORE awaiting `runBatch` so they
 * don't miss the first "queued" → "running" transitions on tight batches.
 *
 * Items are arbitrary strings — file paths, URLs, ticket IDs, etc. When an
 * item happens to be a file path on disk, the backend prepends the first
 * 16 KiB of its content to the substituted prompt automatically. The
 * `{{item}}` token in the prompt template is the substitution sigil.
 */

export type BatchStatus = "queued" | "running" | "done" | "error";

export interface BatchItem {
  index: number;
  item: string;
  status: BatchStatus;
  output: string;
  tokens: number;
  latency_ms: number;
  error: string | null;
}

export interface BatchRunReport {
  run_id: string;
  started_unix_ms: number;
  completed_unix_ms: number;
  items: BatchItem[];
}

export interface BatchProgressEvent {
  item_index: number;
  status: BatchStatus;
  partial_output?: string;
  error?: string;
}

/** Hard ceilings mirrored from the Rust side so the UI can disable inputs
 *  before hitting the backend. */
export const BATCH_MAX_ITEMS = 200;
export const BATCH_MAX_PARALLELISM = 8;
export const BATCH_MIN_PARALLELISM = 1;
export const BATCH_DEFAULT_PARALLELISM = 4;

/**
 * Kick off a batch. Resolves with the final report once every item has
 * either succeeded or errored. The promise never rejects when individual
 * items fail — per-item errors surface on `BatchItem.error`. It only
 * rejects on argument-validation problems (empty items, missing template,
 * over-cap) or a complete gateway outage.
 */
export async function runBatch(
  items: string[],
  promptTemplate: string,
  parallelism?: number,
): Promise<BatchRunReport> {
  return invoke<BatchRunReport>("batch_run", {
    items,
    promptTemplate,
    parallelism: parallelism ?? null,
  });
}

/**
 * Subscribe to live progress for a specific run. Returns the Tauri
 * unlisten function — call it when the modal unmounts or the batch ends
 * so we don't leak listeners.
 *
 * The `runId` isn't known until `runBatch` resolves, but the report
 * carries it; in practice callers subscribe with a wildcard listener via
 * `listenAnyBatchProgress` (below) and filter client-side, or kick off the
 * promise and `await` the run_id from a side channel.
 */
export async function listenBatchProgress(
  runId: string,
  onEvent: (e: BatchProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<BatchProgressEvent>(`batch:progress:${runId}`, (msg) =>
    onEvent(msg.payload),
  );
}

/** Heuristic: does this string look like a unified diff? Same shape as
 *  `git diff` / `diff -u`. The Apply-diffs button checks this before
 *  enabling itself. */
export function looksLikeUnifiedDiff(text: string): boolean {
  if (!text || text.length < 8) return false;
  // `--- a/foo` + `+++ b/foo` is the canonical header pair. A single `@@`
  // hunk header on its own is enough when files are unknown but unlikely
  // to false-positive on chat-style markdown.
  const hasHeader = /^---\s.+\n\+\+\+\s.+$/m.test(text);
  const hasHunk = /^@@\s.*@@/m.test(text);
  return hasHeader && hasHunk;
}

/**
 * Render the run as a markdown document for the "Copy all outputs"
 * button. Mirrors the conversation-export shape — one H3 per item, with
 * the raw output inside a fenced block. Errors get their own section so
 * they don't get lost in the noise.
 */
export function formatBatchAsMarkdown(report: BatchRunReport): string {
  const lines: string[] = [];
  lines.push(`# Batch run \`${report.run_id}\``);
  const dur = Math.max(0, report.completed_unix_ms - report.started_unix_ms);
  lines.push(`_${report.items.length} items in ${(dur / 1000).toFixed(1)}s_`);
  lines.push("");
  for (const it of report.items) {
    lines.push(`### ${it.index + 1}. ${it.item}`);
    if (it.error) {
      lines.push("");
      lines.push(`> error: ${it.error}`);
    } else {
      // Pick a fence longer than the longest backtick run in the output so
      // outputs containing ``` (e.g. nested code blocks) don't break out of
      // the block. CommonMark allows fences of arbitrary length >= 3.
      let maxBackticks = 0;
      for (const match of it.output.matchAll(/`+/g)) {
        maxBackticks = Math.max(maxBackticks, match[0].length);
      }
      const fence = "`".repeat(Math.max(3, maxBackticks + 1));
      lines.push("");
      lines.push(fence);
      lines.push(it.output);
      lines.push(fence);
    }
    lines.push("");
  }
  return lines.join("\n");
}

/** Clamp helper used by the parallelism slider so the UI can't ask the
 *  backend for an out-of-range value. */
export function clampParallelism(n: number): number {
  if (!Number.isFinite(n)) return BATCH_DEFAULT_PARALLELISM;
  return Math.max(
    BATCH_MIN_PARALLELISM,
    Math.min(BATCH_MAX_PARALLELISM, Math.floor(n)),
  );
}

/**
 * Parse the `/batch` slash-command argument.
 *   `/batch a | b | c :: Summarise {{item}}`
 * The items are split on `|`; everything after the first `::` is the
 * prompt template. Whitespace around items + template is trimmed.
 * Returns `null` when the form doesn't match so the caller can fall back
 * to an empty modal.
 */
export function parseBatchSlash(
  raw: string,
): { items: string[]; promptTemplate: string } | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const idx = trimmed.indexOf("::");
  if (idx < 0) {
    // No prompt — return items so the modal can pre-fill the list, prompt
    // stays empty for the user to fill in.
    const items = trimmed
      .split("|")
      .map((s) => s.trim())
      .filter(Boolean);
    return items.length > 0 ? { items, promptTemplate: "" } : null;
  }
  const itemsRaw = trimmed.slice(0, idx);
  const tpl = trimmed.slice(idx + 2).trim();
  const items = itemsRaw
    .split("|")
    .map((s) => s.trim())
    .filter(Boolean);
  if (items.length === 0 && !tpl) return null;
  return { items, promptTemplate: tpl };
}
