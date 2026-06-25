import { invoke } from "@tauri-apps/api/core";

/**
 * Bridge for the REST→MCP tool virtualizer registry. Mirrors the Rust
 * surface in `src-tauri/src/commands/tools.rs`. Stays free of UI state so
 * non-panel callers (e.g. the orchestrator) can hit `invokeTool` directly.
 */

export type ToolMethod = "GET" | "POST" | "PUT" | "DELETE" | "PATCH";
export type InputKind = "string" | "int" | "bool";
export type ResponseFormat = "text" | "json";

export interface ToolInput {
  name: string;
  kind: InputKind;
  required: boolean;
  description: string;
}

export interface ToolDef {
  name: string;
  description: string;
  method: ToolMethod;
  url_template: string;
  inputs: ToolInput[];
  headers: Record<string, string>;
  response_format: ResponseFormat;
  created_unix_ms: number;
  updated_unix_ms: number;
}

export interface ToolInvocationResult {
  ok: boolean;
  status: number | null;
  body: string;
  latency_ms: number;
  error: string | null;
  truncated: boolean;
}

export const TOOL_METHODS: ToolMethod[] = ["GET", "POST", "PUT", "DELETE", "PATCH"];
export const INPUT_KINDS: InputKind[] = ["string", "int", "bool"];

/** A blank tool ready for the "new tool" form. Keeps the panel state pure. */
export function makeEmptyTool(): ToolDef {
  return {
    name: "",
    description: "",
    method: "GET",
    url_template: "https://",
    inputs: [],
    headers: {},
    response_format: "json",
    created_unix_ms: 0,
    updated_unix_ms: 0,
  };
}

export async function listTools(): Promise<ToolDef[]> {
  return invoke<ToolDef[]>("list_tools");
}

export async function getTool(name: string): Promise<ToolDef> {
  return invoke<ToolDef>("get_tool", { name });
}

export async function saveTool(tool: ToolDef): Promise<ToolDef> {
  return invoke<ToolDef>("save_tool", { tool });
}

export async function deleteTool(name: string): Promise<void> {
  return invoke("delete_tool", { name });
}

export async function invokeTool(
  name: string,
  args: Record<string, unknown>,
): Promise<ToolInvocationResult> {
  return invoke<ToolInvocationResult>("invoke_tool", { name, args });
}

export async function testTool(
  name: string,
  args: Record<string, unknown>,
): Promise<ToolInvocationResult> {
  return invoke<ToolInvocationResult>("test_tool", { name, args });
}

/** Tool names double as MCP ids + on-disk filenames — keep them strict. */
export function isValidToolName(name: string): boolean {
  if (!name || name.length > 64) return false;
  return /^[A-Za-z0-9_.\-]+$/.test(name);
}

/** Coerce a raw string from a form field into the typed value the backend
 *  wants, matching the input's declared `kind`. Empty strings stay empty
 *  for `string`-typed slots so the user can fire optional GETs with bare
 *  templates. */
export function coerceArg(input: ToolInput, raw: string): unknown {
  if (input.kind === "int") {
    const n = Number.parseInt(raw, 10);
    return Number.isFinite(n) ? n : raw;
  }
  if (input.kind === "bool") {
    return raw === "true" || raw === "1" || raw === "yes";
  }
  return raw;
}
