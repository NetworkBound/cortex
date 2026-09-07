import { invoke } from "@tauri-apps/api/core";

/**
 * Cost-per-success (outcome-aware) routing — issue 006 bindings. Mirrors
 * `get_outcome_routing` / `set_outcome_routing` in
 * `src-tauri/src/commands/settings.rs`.
 *
 * DEFAULT-OFF (`~/.cortex/outcome-routing.json` absent = off). When on, and
 * ONLY when a message has no explicit agent or model pick, the router's
 * default branch prefers the provider with the best recent
 * success-rate-per-dollar (from the local Reliability aggregates), gated by a
 * minimum run count and a recency window. Thin data, explicit picks, model
 * routes, and the CLI/safety branches behave exactly as with the toggle off.
 */
export async function getOutcomeRouting(): Promise<boolean> {
  return invoke<boolean>("get_outcome_routing");
}

/** Toggle outcome-aware routing; applies from the next message. */
export async function setOutcomeRouting(enabled: boolean): Promise<boolean> {
  return invoke<boolean>("set_outcome_routing", { enabled });
}
