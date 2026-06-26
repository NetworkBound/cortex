// PRP (Product Requirement Prompt) frontend bridge.
//
// Mirrors the Rust types in `src-tauri/src/prp/`. The PRPPanel uses these
// helpers; nothing else in the app should reach into `invoke()` for PRP
// commands directly.
//
// All calls degrade to safe fallbacks (`[]` / `null`) when running outside
// Tauri (e.g. in the storybook), so the panel renders an empty state instead
// of crashing the React tree.

import { invoke } from "@tauri-apps/api/core";

export type PrpStage = "stage-1" | "stage-2" | "stage-3" | "stage-4";

export type GateVerdict = "pass" | "fail" | "skipped" | "pending";

/** Map of `gate name → verdict string`. Keys: syntax/tests/coverage/build/security. */
export type GateStatuses = Record<string, GateVerdict | string>;

export interface Prp {
  name: string;
  status: PrpStage;
  created_unix_ms: number;
  stages: string[];
  gates: GateStatuses;
  body: string;
  path: string;
}

export interface PrpProgress {
  name: string;
  status: PrpStage;
  created_unix_ms: number;
  gates: GateStatuses;
  gates_resolved: number;
  gates_passed: number;
}

export interface GateResult {
  name: string;
  verdict: "pass" | "fail" | "skipped";
  message: string;
  duration_ms: number;
}

export interface ValidationReport {
  prp_name: string;
  gates: GateResult[];
}

export async function listPrps(projectRoot: string): Promise<Prp[]> {
  try {
    return (await invoke<Prp[]>("list_prps", { projectRoot })) ?? [];
  } catch (err) {
    console.warn("list_prps failed", err);
    return [];
  }
}

export async function getPrp(projectRoot: string, name: string): Promise<Prp | null> {
  try {
    return (await invoke<Prp | null>("get_prp", { projectRoot, name })) ?? null;
  } catch (err) {
    console.warn("get_prp failed", err);
    return null;
  }
}

export async function createPrp(
  projectRoot: string,
  name: string,
  bodyHint?: string,
): Promise<Prp> {
  return await invoke<Prp>("create_prp", { projectRoot, name, bodyHint: bodyHint ?? "" });
}

export async function advancePrpStage(
  projectRoot: string,
  name: string,
  stage?: PrpStage,
): Promise<Prp> {
  return await invoke<Prp>("advance_prp_stage", { projectRoot, name, stage: stage ?? null });
}

export async function runPrpGates(
  projectRoot: string,
  name: string,
): Promise<ValidationReport> {
  return await invoke<ValidationReport>("run_prp_gates", { projectRoot, name });
}

export async function prpProgress(projectRoot: string): Promise<PrpProgress[]> {
  try {
    return (await invoke<PrpProgress[]>("prp_progress", { projectRoot })) ?? [];
  } catch (err) {
    console.warn("prp_progress failed", err);
    return [];
  }
}

/** Human-facing label for a stage value. Falls back to the raw string. */
export function stageLabel(stage: PrpStage | string): string {
  switch (stage) {
    case "stage-1":
      return "Spec drafted";
    case "stage-2":
      return "Plan validated";
    case "stage-3":
      return "Implementation";
    case "stage-4":
      return "Test + verify";
    default:
      return String(stage);
  }
}

/** Short prefix (e.g. "1/4") for use in compact badges. */
export function stageOrdinal(stage: PrpStage | string): string {
  const m = String(stage).match(/stage-(\d+)/);
  return m ? `${m[1]}/4` : "—";
}
