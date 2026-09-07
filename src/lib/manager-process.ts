import { invoke } from "@tauri-apps/api/core";

/**
 * Mirror of `commands::manager_process` Rust types. Subtask status is a
 * stringly-typed enum: pending | running | validating | done | failed.
 *
 * The lifecycle:
 *   - `managerDecompose(goal)` → manager LLM returns an ordered plan.
 *   - `managerRunStep(plan_id, i)` → run the subtask + auto-validate.
 *   - `managerValidate(plan_id, i, output)` → standalone validate hook.
 *
 * Plans are NOT persisted between launches; the in-memory registry expires
 * each plan one hour after the last access.
 */

export type SubtaskStatus =
  | "pending"
  | "running"
  | "validating"
  | "done"
  | "failed";

export interface Subtask {
  name: string;
  role: string;
  prompt: string;
  depends_on: number[];
  status: SubtaskStatus;
  output?: string | null;
}

export interface Plan {
  plan_id: string;
  goal: string;
  subtasks: Subtask[];
  created_unix_ms: number;
}

export interface Validation {
  ok: boolean;
  reason: string;
}

export interface StepResult {
  output: string;
  validation: Validation;
}

/** Ask the manager LLM to decompose a goal into role-tagged subtasks. */
export async function managerDecompose(goal: string): Promise<Plan> {
  return invoke<Plan>("manager_decompose", { goal });
}

/** Run the subtask at `stepIndex`. The backend auto-validates and flips the
 *  per-step status as it goes (pending → running → validating → done/failed). */
export async function managerRunStep(
  planId: string,
  stepIndex: number,
): Promise<StepResult> {
  return invoke<StepResult>("manager_run_step", {
    planId,
    stepIndex,
  });
}

/** Standalone validate. Useful when the UI has the output (e.g. user pasted
 *  one) but didn't go through `manager_run_step` to get it. */
export async function managerValidate(
  planId: string,
  stepIndex: number,
  output: string,
): Promise<Validation> {
  return invoke<Validation>("manager_validate", {
    planId,
    stepIndex,
    output,
  });
}

/** Human-readable label for a status pill. */
export function statusLabel(s: SubtaskStatus): string {
  switch (s) {
    case "pending":
      return "pending";
    case "running":
      return "running…";
    case "validating":
      return "validating…";
    case "done":
      return "done";
    case "failed":
      return "failed";
  }
}
