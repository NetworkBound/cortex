// Workflow templates — preset multi-step recipes stored as YAML in
// `~/.cortex/workflows/<name>.yaml`. Mirrors the on-disk schema enforced by
// `commands/workflows.rs`.
//
// Storage shape (per file):
//   name: string
//   description?: string
//   steps: [{ role, prompt }, ...]
//
// Run semantics are fire-and-forget: `runWorkflow(name)` resolves the
// workflow on the backend and returns a `WorkflowRun` whose `steps` the
// caller iterates through, appending one chat message per step. The actual
// chat dispatch is intentionally kept on the frontend so we don't fight the
// existing streaming pipeline for run-id ordering.
//
// All commands degrade gracefully: a missing workflows dir surfaces as an
// empty list rather than throwing, so a freshly-installed Cortex still
// renders an empty WorkflowsPanel without an error toast.

import { invoke } from "@tauri-apps/api/core";

export interface WorkflowStep {
  role: string;
  prompt: string;
}

export interface Workflow {
  name: string;
  description?: string | null;
  steps: WorkflowStep[];
}

export interface WorkflowRun {
  run_id: string;
  name: string;
  steps: WorkflowStep[];
  started_unix_ms: number;
}

export async function listWorkflows(): Promise<Workflow[]> {
  try {
    const out = await invoke<Workflow[]>("list_workflows");
    // Defensive: backend already returns sorted, but the type allows
    // anything so normalise here too.
    return Array.isArray(out) ? out : [];
  } catch (err) {
    console.warn("listWorkflows failed", err);
    return [];
  }
}

export async function getWorkflow(name: string): Promise<Workflow | null> {
  try {
    return await invoke<Workflow>("get_workflow", { name });
  } catch (err) {
    console.warn("getWorkflow failed", err);
    return null;
  }
}

export async function saveWorkflow(workflow: Workflow): Promise<Workflow | null> {
  try {
    return await invoke<Workflow>("save_workflow", { workflow });
  } catch (err) {
    console.warn("saveWorkflow failed", err);
    return null;
  }
}

export async function deleteWorkflow(name: string): Promise<boolean> {
  try {
    await invoke("delete_workflow", { name });
    return true;
  } catch (err) {
    console.warn("deleteWorkflow failed", err);
    return false;
  }
}

export async function runWorkflow(name: string): Promise<WorkflowRun | null> {
  try {
    return await invoke<WorkflowRun>("run_workflow", { name });
  } catch (err) {
    console.warn("runWorkflow failed", err);
    return null;
  }
}

/**
 * Format a workflow step as a chat-ready prompt with a role prefix. Keeps
 * the persona visible in the transcript so the user can tell which step
 * produced which response without leaving the chat view.
 */
export function formatStepPrompt(step: WorkflowStep): string {
  return `[role:${step.role}] ${step.prompt}`;
}
