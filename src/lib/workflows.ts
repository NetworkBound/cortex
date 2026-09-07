// Workflow templates — preset multi-step recipes stored as YAML in
// `~/.cortex/workflows/<name>.yaml`. Mirrors the on-disk schema enforced by
// `commands/workflows.rs`.
//
// Storage shape (per file):
//   name: string
//   description?: string
//   inputs?: [{ key, label?, default?, required? }, ...]   (v2)
//   steps: [{ role, prompt, model?, pipe_output? }, ...]
//
// Run semantics are fire-and-forget: `runWorkflow(name, inputs?)` resolves
// the workflow on the backend (validating required inputs and expanding
// `{{key}}` placeholders server-side) and returns a `WorkflowRun` whose
// `steps` the caller iterates through, appending one chat message per step.
// The actual chat dispatch is intentionally kept on the frontend so we don't
// fight the existing streaming pipeline for run-id ordering. A step's
// `model` rides the existing per-call model override; `pipe_output` means
// the NEXT step's prompt gets this step's answer prefixed at dispatch time
// (see `buildPipedPrompt`).
//
// All commands degrade gracefully: a missing workflows dir surfaces as an
// empty list rather than throwing, so a freshly-installed Cortex still
// renders an empty WorkflowsPanel without an error toast.
//
// `exportWorkflowYaml` / `importWorkflowFromPath` (full scope) round-trip a
// single workflow as a `.yaml` file via the native file dialogs — these two
// throw on failure instead of swallowing to null, since the panel wants the
// real backend message ("already exists", "invalid workflow YAML").

import { invoke } from "@tauri-apps/api/core";
import { promptDialog } from "@/lib/dialogs";

export interface WorkflowStep {
  role: string;
  prompt: string;
  /** v2: optional per-step model override (e.g. `ollama:llama3.1` or a
   *  gateway model id). Routed through the existing chat model override. */
  model?: string | null;
  /** v2: when true, this step's answer prefixes the next step's prompt. */
  pipe_output?: boolean;
}

/** v2: a declared `{{key}}` template input, collected pre-run. */
export interface WorkflowInput {
  key: string;
  label?: string | null;
  default?: string | null;
  required?: boolean;
}

export interface Workflow {
  name: string;
  description?: string | null;
  /** v2: declared template inputs. Absent/empty for v1 workflows. */
  inputs?: WorkflowInput[];
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

export async function runWorkflow(
  name: string,
  inputs?: Record<string, string>,
): Promise<WorkflowRun | null> {
  try {
    return await invoke<WorkflowRun>("run_workflow", { name, inputs: inputs ?? null });
  } catch (err) {
    console.warn("runWorkflow failed", err);
    return null;
  }
}

/**
 * Export a workflow as a YAML string (same shape `save_workflow` persists —
 * a v1 workflow round-trips with no v2 keys present). Unlike the other
 * helpers above, this one THROWS on failure rather than swallowing to
 * `null`/`[]`: import/export errors ("not found") are specific and worth
 * surfacing verbatim to the user, so callers catch and show `humanizeError`.
 */
export async function exportWorkflowYaml(name: string): Promise<string> {
  return await invoke<string>("export_workflow", { name });
}

/**
 * Import a workflow from a YAML file at `path` (resolved by the caller via
 * the native file-open dialog). Refuses to clobber an existing workflow of
 * the same name — throws so the panel can surface the real backend message
 * (e.g. "a workflow named 'x' already exists…", "invalid workflow YAML").
 */
export async function importWorkflowFromPath(path: string): Promise<Workflow> {
  return await invoke<Workflow>("import_workflow", { path });
}

/**
 * Collect values for a workflow's declared inputs via sequential in-app
 * prompt dialogs (the pre-run form). Defaults are pre-filled; a required
 * input re-prompts while blank. Resolves `null` when the user cancels —
 * callers must abort the run. Workflows without inputs resolve `{}`
 * immediately so v1 runs never see a dialog.
 */
export async function collectWorkflowInputs(
  workflow: Workflow,
): Promise<Record<string, string> | null> {
  const declared = workflow.inputs ?? [];
  const values: Record<string, string> = {};
  for (const input of declared) {
    const label = input.label?.trim() || input.key;
    for (;;) {
      const value = await promptDialog({
        title: `Run ${workflow.name}`,
        message: input.required ? `${label} (required)` : label,
        initialValue: input.default ?? "",
        confirmLabel: "Continue",
      });
      if (value === null) return null;
      if (input.required && value.trim().length === 0) continue;
      values[input.key] = value;
      break;
    }
  }
  return values;
}

/**
 * Format a workflow step as a chat-ready prompt with a role prefix. Keeps
 * the persona visible in the transcript so the user can tell which step
 * produced which response without leaving the chat view. Steps pinned to a
 * model (v2) also surface the override so the transcript shows where the
 * step will route; v1 steps render exactly as before.
 */
export function formatStepPrompt(step: WorkflowStep): string {
  const tag = step.model ? `[role:${step.role} · model:${step.model}]` : `[role:${step.role}]`;
  return `${tag} ${step.prompt}`;
}

/**
 * Compose the prompt for a step that follows a `pipe_output` step: the
 * previous step's captured answer is prefixed above the step's own prompt.
 * Pure string composition — used at dispatch time and by the run-queue
 * notes so the user sees the exact shape the next prompt will take.
 */
export function buildPipedPrompt(previousOutput: string, prompt: string): string {
  return `Previous step output:\n\n${previousOutput}\n\n---\n\n${prompt}`;
}
