/**
 * Spend budget for the chat session.
 *
 * Cortex already *tracks* USD spend (see `cost-tracker.ts` / the Usage tab);
 * this is the ceiling: a single USD number persisted in `localStorage`, set
 * via `/budget <usd>` and ENFORCED in ChatPane's send path — a one-time toast
 * when spend crosses 80% of the cap, and an explicit confirm required for
 * every send past 100%. `evaluateBudget` fails open (returns null) when the
 * cost estimate is unavailable, so a tracing hiccup can never block the chat
 * loop.
 */

const KEY = "cortex.budget.capUsd";

/** The current soft cap in USD, or `null` when unset / malformed. */
export function getBudgetCap(): number | null {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw === null) return null;
    const n = Number.parseFloat(raw);
    return Number.isFinite(n) && n > 0 ? n : null;
  } catch {
    // localStorage can throw in locked-down webviews — treat as unset.
    return null;
  }
}

/** Persist a positive USD cap. No-ops (returns false) on a non-positive value. */
export function setBudgetCap(usd: number): boolean {
  if (!Number.isFinite(usd) || usd <= 0) return false;
  try {
    localStorage.setItem(KEY, String(usd));
    return true;
  } catch {
    return false;
  }
}

/** Remove the cap. */
export function clearBudgetCap(): void {
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* ignore */
  }
}

export type BudgetLevel = "ok" | "warn" | "over";

/** Classify spend against a cap: <80% ok, ≥80% warn, ≥100% over. */
export function budgetLevel(spentUsd: number, capUsd: number): BudgetLevel {
  if (capUsd <= 0) return "ok";
  const pct = spentUsd / capUsd;
  if (pct >= 1) return "over";
  if (pct >= 0.8) return "warn";
  return "ok";
}

export interface BudgetStatus {
  cap: number;
  spent: number;
  level: BudgetLevel;
  /** spent / cap, unclamped (1.4 = 140% over). */
  pct: number;
}

/**
 * Evaluate current spend against the cap. Returns `null` when no cap is set,
 * and ALSO null when the cost estimate fails — callers gate sends on this, so
 * it must fail open rather than wedge the composer on a tracing-store error.
 * The `localStorage` cap read is sync, so the (local) `cost_estimate` invoke
 * only happens when a cap is actually set.
 */
export async function evaluateBudget(sessionId?: string): Promise<BudgetStatus | null> {
  const cap = getBudgetCap();
  if (cap === null) return null;
  try {
    const { estimateCost } = await import("./cost-tracker");
    const report = await estimateCost(sessionId);
    const spent = report.total_usd;
    return { cap, spent, level: budgetLevel(spent, cap), pct: spent / cap };
  } catch {
    return null;
  }
}
