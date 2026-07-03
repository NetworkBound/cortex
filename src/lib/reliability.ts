import { invoke } from "@tauri-apps/api/core";

/**
 * Typed bridge for the Agent Reliability Dashboard. Mirrors the Rust
 * `ReliabilityReport`/`ReliabilityRow` (snake_case) from
 * `observability::tracing_store`. All values are a LOCAL VIEW aggregated from
 * the on-device trace store — success is derived from span status + error
 * events, gateway-internal retries are invisible, and cost is a local estimate.
 */

export interface ReliabilityRow {
  /** Display key: the provider/agent id (provider rows) or model (model rows). */
  key: string;
  agent_id: string | null;
  model: string | null;
  runs: number;
  ok_runs: number;
  error_runs: number;
  /** Finished-but-unknown / still-running spans (excluded from success_rate). */
  running_runs: number;
  /** ok_runs / (ok_runs + error_runs); 0 when nothing finished. */
  success_rate: number;
  p50_ms: number | null;
  p95_ms: number | null;
  avg_ms: number | null;
  total_tokens: number;
  est_usd: number;
  top_error_class: string | null;
  last_run_ms: number;
}

export interface ReliabilityReport {
  since_ms: number | null;
  generated_ms: number;
  totals: ReliabilityRow;
  by_provider: ReliabilityRow[];
  by_model: ReliabilityRow[];
}

/**
 * Aggregate run outcomes over the last `windowHours` (undefined / 0 = all time).
 */
export async function reliabilitySummary(
  windowHours?: number,
): Promise<ReliabilityReport> {
  return invoke<ReliabilityReport>("reliability_summary", {
    windowHours: windowHours ?? null,
  });
}

/** Render a set of rows as CSV (for the export button). */
export function rowsToCsv(rows: ReliabilityRow[]): string {
  const header = [
    "key",
    "runs",
    "ok_runs",
    "error_runs",
    "success_rate",
    "p50_ms",
    "p95_ms",
    "avg_ms",
    "total_tokens",
    "est_usd",
    "top_error_class",
    "last_run_ms",
  ];
  const cell = (v: string | number | null): string => {
    if (v === null || v === undefined) return "";
    const s = String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const lines = rows.map((r) =>
    [
      r.key,
      r.runs,
      r.ok_runs,
      r.error_runs,
      r.success_rate.toFixed(4),
      r.p50_ms,
      r.p95_ms,
      r.avg_ms,
      r.total_tokens,
      r.est_usd.toFixed(6),
      r.top_error_class,
      r.last_run_ms,
    ]
      .map(cell)
      .join(","),
  );
  return [header.join(","), ...lines].join("\n");
}
