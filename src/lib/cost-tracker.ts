import { invoke } from "@tauri-apps/api/core";

/**
 * Per-session cost row. `prompt_tokens` / `completion_tokens` are estimates —
 * the tracing store only records a single `total` per run today, so the
 * backend splits 50/50 until a richer token schema lands.
 */
export interface CostBySession {
  session_id: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  usd: number;
  last_active_ms: number;
}

/** Per-model cost row, priced from a hard-coded 2026-defaults pricing table. */
export interface CostByModel {
  model: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  usd: number;
  input_price_per_million_usd: number;
  output_price_per_million_usd: number;
}

/** Mirrors `src-tauri::commands::cost_tracker::CostReport`. */
export interface CostReport {
  total_usd: number;
  by_session: CostBySession[];
  by_model: CostByModel[];
  generated_unix_ms: number;
}

/**
 * Estimate USD spend across the local tracing store. Pass a `sessionId` to
 * scope the `by_session` array (and headline `total_usd`) to a single chat.
 *
 * Prices come from a hardcoded table in the Rust crate — see
 * `commands/cost_tracker.rs::PRICING`. Unknown models fall back to a generic
 * `$1.0 / $4.0` per-million default.
 */
export async function estimateCost(sessionId?: string): Promise<CostReport> {
  return invoke<CostReport>("cost_estimate", {
    args: { session_id: sessionId ?? null },
  });
}

/**
 * Format a USD value for chat-message display. Uses 4dp under $0.01 so we
 * don't round trivially small spends to $0.00.
 */
export function formatUsd(value: number): string {
  if (!Number.isFinite(value)) return "$0.00";
  if (value === 0) return "$0.00";
  if (Math.abs(value) < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(2)}`;
}
