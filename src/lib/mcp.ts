/**
 * Thin TS wrappers around the `mcp_*` Tauri commands.
 *
 * The backend owns persistence + the live MCP client; this side just forwards
 * typed args and returns the backend's responses. Tauri maps the camelCase JS
 * arg keys (`server`, `id`, `tool`, `args`) onto the Rust snake_case params.
 *
 * Callers should wrap these in try/catch and surface failures via a toast —
 * during dev the commands may reject if the backend half isn't wired yet.
 */
import { invoke } from "@tauri-apps/api/core";

/**
 * How much a server's tools are trusted to run. Mirrors the backend's
 * `McpTrustLevel`: `trusted` runs without per-call confirmation, `ask`
 * (the default) requires an explicit confirmation for every call, and
 * `untrusted` refuses tool calls outright.
 */
export type McpTrustLevel = "trusted" | "ask" | "untrusted";

/** A configured MCP server (stdio launch spec + enabled flag). */
export interface McpServerConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  enabled: boolean;
  /**
   * Per-server environment variables layered onto the spawned process env.
   * Values are always user-supplied — catalog entries declare the *keys* a
   * server needs (e.g. a token) but never carry the secret itself.
   */
  env?: Record<string, string>;
  /** Trust level gating tool calls. Absent (older configs) means "ask". */
  trust?: McpTrustLevel;
  /** Tool names the user switched off for this server. Absent = all on. */
  disabledTools?: string[];
  /**
   * Issue 009 full scope: per-tool call-rate ceiling, `tool name -> max
   * calls per rolling 60s window`. A tool with no entry is unlimited
   * (the default — absent/empty means zero behavior change).
   */
  rateLimits?: Record<string, number>;
  /**
   * Advertise this server's enabled tools to the model during chat turns
   * (issue 008). Absent/false (the default) exposes nothing — every call the
   * model then makes still runs the trust/sandbox/guardrail pipeline.
   */
  exposeInChat?: boolean;
}

/** A tool exposed by a connected MCP server. */
export interface McpTool {
  name: string;
  description?: string;
  inputSchema?: unknown;
}

/** List every configured server. */
export async function listMcpServers(): Promise<McpServerConfig[]> {
  return await invoke<McpServerConfig[]>("mcp_list_servers");
}

/** Upsert a server; returns the full refreshed list. */
export async function saveMcpServer(
  server: McpServerConfig,
): Promise<McpServerConfig[]> {
  return await invoke<McpServerConfig[]>("mcp_save_server", { server });
}

/** Delete a server by id; returns the full refreshed list. */
export async function deleteMcpServer(id: string): Promise<McpServerConfig[]> {
  return await invoke<McpServerConfig[]>("mcp_delete_server", { id });
}

/** Connect to a server and return its advertised tools. */
export async function connectMcp(id: string): Promise<McpTool[]> {
  return await invoke<McpTool[]>("mcp_connect", { id });
}

/** Disconnect from a server. */
export async function disconnectMcp(id: string): Promise<void> {
  await invoke<void>("mcp_disconnect", { id });
}

/**
 * Which launcher commands (`npx`, `uvx`, …) resolve on PATH right now.
 * Preflight for the catalog: a server whose runtime is missing would add
 * fine and then fail opaquely at its first tool call.
 */
export async function probeMcpRuntimes(
  commands: string[],
): Promise<Record<string, boolean>> {
  return await invoke<Record<string, boolean>>("mcp_probe_runtimes", {
    commands,
  });
}

/**
 * Invoke a tool on a connected server; returns the stringified result.
 * `approved` must be true only when the user explicitly confirmed THIS call
 * (required for servers whose trust level is "ask"; ignored for "trusted";
 * "untrusted" servers refuse regardless).
 */
export async function callMcpTool(
  id: string,
  tool: string,
  args?: unknown,
  approved?: boolean,
): Promise<string> {
  return await invoke<string>("mcp_call_tool", { id, tool, args, approved });
}
