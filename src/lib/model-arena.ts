import { invoke } from "@tauri-apps/api/core";

/** One model's transcript for a single arena send. `error` is non-null iff
 *  the gateway call failed; `response` may still hold partial output. */
export interface ModelTurn {
  model: string;
  /** The adapter that served this leg (`claude-cli` / `ollama` /
   *  `gateway-remote`). Empty when no adapter could be resolved. */
  adapter: string;
  response: string;
  tokens: number;
  latency_ms: number;
  error: string | null;
}

/** Aggregate result of one arena send across N models. */
export interface ArenaRun {
  run_id: string;
  models: ModelTurn[];
}

/** On-disk ELO row mirrored from the backend. */
export interface ModelRating {
  model: string;
  rating: number;
  wins: number;
  losses: number;
  total_runs: number;
}

/** Backend response after applying a vote. Carries the full updated table so
 *  the leaderboard sidebar can render without an extra roundtrip. */
export interface EloUpdate {
  ratings: ModelRating[];
}

/** Send one prompt to 2-4 models in parallel and collect each response. */
export async function arenaSend(prompt: string, models: string[]): Promise<ArenaRun> {
  return invoke<ArenaRun>("arena_send", { prompt, models });
}

/** Record a winner against the given losers — applies a K=32 ELO update
 *  per (winner, loser) pair and persists the new ratings to disk. */
export async function arenaVote(
  runId: string,
  winner: string,
  losers: string[],
): Promise<EloUpdate> {
  return invoke<EloUpdate>("arena_vote", { runId, winner, losers });
}

/** Fetch the full ELO leaderboard sorted by rating descending. */
export async function arenaLeaderboard(): Promise<ModelRating[]> {
  return invoke<ModelRating[]>("arena_leaderboard");
}

/** Human-readable W-L cell for the sidebar table. */
export function formatRecord(r: ModelRating): string {
  return `${r.wins}-${r.losses}`;
}

/** Compact latency label, e.g. 850 → "850ms", 2400 → "2.4s". */
export function formatLatency(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  return s % 1 === 0 ? `${s.toFixed(0)}s` : `${s.toFixed(1)}s`;
}
