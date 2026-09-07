import { invoke } from "@tauri-apps/api/core";

/**
 * Budget ceilings per session — issue 006 full-scope bindings. Mirrors
 * `get_session_budget` / `set_session_budget` in
 * `src-tauri/src/commands/settings.rs`.
 *
 * OPTIONAL, per-session USD spend cap (`cap_usd: null` = no cap, the
 * default — every session behaves exactly as it does today). When a cap is
 * set and this session's total estimated spend (from local Reliability/Usage
 * data) reaches it, `chat_send` blocks further bare (no explicit agent/model)
 * messages with an error until the cap is raised or cleared. As spend
 * approaches the cap, outcome-aware routing (when enabled) starts preferring
 * cheaper reliable providers over the raw cost-per-success winner. Explicit
 * agent picks and model routes never consult a budget cap.
 */
export async function getSessionBudget(sessionId: string): Promise<number | null> {
  return invoke<number | null>("get_session_budget", { sessionId });
}

/** Set (or clear, with `null`) the spend cap for one session. */
export async function setSessionBudget(
  sessionId: string,
  capUsd: number | null,
): Promise<number | null> {
  return invoke<number | null>("set_session_budget", { sessionId, capUsd });
}
