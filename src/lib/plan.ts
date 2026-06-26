import { invoke } from "@tauri-apps/api/core";

/**
 * Structured plan emitted by the agent in plan-mode. Mirrors the shape we
 * accept on the wire — every field except `steps` is optional so partial
 * plans still render.
 *
 * Detection priority (used by `extractPlan` below):
 *   1. A tool-call event with `name === "plan"` whose args parse to this shape.
 *   2. An assistant message whose `content` is JSON with a top-level `plan`
 *      field, OR an object that already matches `Plan` itself.
 */
export interface PlanStep {
  /** Short imperative title, e.g. "Add PlanCard component". */
  title: string;
  /** Optional longer description rendered as muted body text. */
  detail?: string;
  /** Estimated wall-clock cost, free-form (e.g. "2m", "~5min"). */
  estimated_time?: string;
}

export interface Plan {
  /** Stable id the orchestrator uses to correlate approval events. */
  id: string;
  /** Plan headline, rendered as the card title. */
  title: string;
  /** Optional one-line summary under the title. */
  summary?: string;
  steps: PlanStep[];
  /** Aggregate estimates shown in the footer when present. */
  estimated_time?: string;
  estimated_cost?: string;
}

/**
 * Sniff a plan out of arbitrary content. Accepts the structured object, a
 * JSON string, or an object wrapping it under `{ plan: ... }`. Returns
 * `null` when the input doesn't look like a plan — never throws, since this
 * runs on every assistant message.
 */
export function extractPlan(raw: unknown): Plan | null {
  if (raw == null) return null;
  let candidate: unknown = raw;
  if (typeof raw === "string") {
    const trimmed = raw.trim();
    if (!trimmed.startsWith("{")) return null;
    try {
      candidate = JSON.parse(trimmed);
    } catch {
      return null;
    }
  }
  if (typeof candidate !== "object" || candidate === null) return null;
  const obj = candidate as Record<string, unknown>;
  const nested = obj.plan;
  if (nested && typeof nested === "object") {
    return asPlan(nested as Record<string, unknown>);
  }
  return asPlan(obj);
}

function asPlan(obj: Record<string, unknown>): Plan | null {
  const steps = obj.steps;
  if (!Array.isArray(steps) || steps.length === 0) return null;
  const normalizedSteps: PlanStep[] = [];
  for (const s of steps) {
    if (typeof s === "string") {
      normalizedSteps.push({ title: s });
      continue;
    }
    if (s && typeof s === "object") {
      const o = s as Record<string, unknown>;
      const title = typeof o.title === "string"
        ? o.title
        : typeof o.name === "string"
          ? o.name
          : null;
      if (!title) continue;
      normalizedSteps.push({
        title,
        detail: typeof o.detail === "string" ? o.detail : undefined,
        estimated_time: typeof o.estimated_time === "string"
          ? o.estimated_time
          : undefined,
      });
    }
  }
  if (normalizedSteps.length === 0) return null;
  const id = typeof obj.id === "string" && obj.id.trim().length > 0
    ? obj.id
    : `plan-${Date.now().toString(36)}`;
  const title = typeof obj.title === "string" ? obj.title : "Proposed plan";
  return {
    id,
    title,
    summary: typeof obj.summary === "string" ? obj.summary : undefined,
    steps: normalizedSteps,
    estimated_time: typeof obj.estimated_time === "string"
      ? obj.estimated_time
      : undefined,
    estimated_cost: typeof obj.estimated_cost === "string"
      ? obj.estimated_cost
      : undefined,
  };
}

/**
 * Approve a plan — backend emits a `plan_approved` event on the session
 * channel so the orchestrator can advance into act-mode. Pure fire-and-forget,
 * resolves once the event has been dispatched.
 */
export async function approvePlan(
  sessionId: string,
  planId: string,
): Promise<void> {
  return invoke("approve_plan", {
    args: { session_id: sessionId, plan_id: planId },
  });
}
