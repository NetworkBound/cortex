import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Risk = "low" | "medium" | "high";

export interface AgentDescriptor {
  id: string;
  label: string;
  description: string;
  capabilities: string[];
  available: boolean;
}

export interface ChatTurn {
  role: "user" | "assistant" | "system";
  content: string;
  agent?: string;
}

export type AgentEvent =
  | { type: "started"; agent_id: string; run_id: string | null }
  | { type: "token"; delta: string }
  | { type: "reasoning"; text: string }
  | { type: "tool_call"; name: string; args: unknown; preview: string | null }
  | { type: "tool_result"; name: string; ok: boolean; summary: string; duration_ms: number | null }
  | { type: "file_edit"; path: string; lines_changed: number }
  | { type: "approval_request"; run_id: string; tool: string | null; preview: string | null; choices: string[]; request: unknown }
  | { type: "approval_resolved"; run_id: string; choice: string }
  | { type: "error"; message: string }
  | { type: "done"; total_tokens: number | null; run_id: string | null }
  | { type: "orchestrator_route"; agents: string[]; reason: string };

export interface AgentEventEnvelope {
  agent_id?: string;
  event?: AgentEvent;
  type?: string;
  agents?: string[];
  reason?: string;
}

export type Mode = "plan" | "act";

export interface ChatSendArgs {
  sessionId: string;
  message: string;
  agent?: string;
  projectRoot?: string;
  history?: ChatTurn[];
  /** Override the mode from the store. Defaults to the current store value. */
  mode?: Mode;
  /** Base64 image data URIs (anthropic vision format). See composer-drop.ts. */
  images?: string[];
  /** Aider-style two-phase plan→edit. Defaults to the store's `architectMode`. */
  architectMode?: boolean;
  /** Model override for the planner phase (only honored when architectMode). */
  plannerModel?: string | null;
  /** Model override for the editor phase (only honored when architectMode). */
  editorModel?: string | null;
  /** Per-prompt model override (wins over the global gateway model). Defaults
   *  to the store's `selectedModel`. */
  model?: string | null;
  /** Per-prompt reasoning-effort override (`minimal | low | medium | high`,
   *  Codex CLI parity). Wins over the global config default; an unrecognized
   *  value falls through to it. Defaults to the store's
   *  `selectedReasoningEffort`. `null` => use the global default. */
  reasoningEffort?: string | null;
}

export interface ChatSendResult {
  session_id: string;
  picked_agents: string[];
  routing_reason: string;
  /** @-tokens the backend resolved into inline attachments. */
  attachments?: string[];
}

/**
 * Reads the current Plan/Act mode from `localStorage` (where the Zustand
 * store persists every toggle). This avoids an import cycle with `store.ts`,
 * and falls back to `"act"` outside a browser context.
 */
function modeFromStorage(): Mode {
  try {
    return localStorage.getItem("cortex.mode") === "plan" ? "plan" : "act";
  } catch {
    return "act";
  }
}

/** Same localStorage-as-store pattern as `modeFromStorage` to keep this
 *  module free of the `@/state/store` import cycle. */
function architectFromStorage(): { architect: boolean; planner: string | null; editor: string | null } {
  try {
    return {
      architect: localStorage.getItem("cortex.architectMode") === "true",
      planner: localStorage.getItem("cortex.plannerModel"),
      editor: localStorage.getItem("cortex.editorModel"),
    };
  } catch {
    return { architect: false, planner: null, editor: null };
  }
}

/** Per-prompt model override persisted by the store. Same localStorage trick
 *  to dodge the `@/state/store` import cycle. `null` => "Auto (gateway)". */
function modelFromStorage(): string | null {
  try {
    return localStorage.getItem("cortex.selectedModel");
  } catch {
    return null;
  }
}

/** Per-prompt reasoning-effort override persisted by the store. Same
 *  localStorage trick to dodge the `@/state/store` import cycle. `null` =>
 *  fall back to the global config default. */
function reasoningEffortFromStorage(): string | null {
  try {
    return localStorage.getItem("cortex.selectedReasoningEffort");
  } catch {
    return null;
  }
}

export async function chatSend(args: ChatSendArgs): Promise<ChatSendResult> {
  const mode = args.mode ?? modeFromStorage();
  const a = architectFromStorage();
  const architectMode = args.architectMode ?? a.architect;
  const plannerModel = args.plannerModel ?? a.planner;
  const editorModel = args.editorModel ?? a.editor;
  const model = args.model ?? modelFromStorage() ?? null;
  const reasoningEffort = args.reasoningEffort ?? reasoningEffortFromStorage() ?? null;
  return invoke<ChatSendResult>("chat_send", {
    args: {
      session_id: args.sessionId,
      message: args.message,
      agent: args.agent ?? null,
      project_root: args.projectRoot ?? null,
      history: args.history ?? [],
      mode,
      images: args.images ?? [],
      architect_mode: architectMode,
      planner_model: plannerModel,
      editor_model: editorModel,
      model,
      reasoning_effort: reasoningEffort,
    },
  });
}

export async function setCurrentMode(mode: Mode): Promise<void> {
  return invoke("set_current_mode", { mode });
}

export interface ApproveRunOptions {
  /** Replacement for the tool's args. Set when the user edits a shell
   *  command in the approval prompt — the gateway substitutes server-side. */
  editedPayload?: unknown;
  /** 0-based indices of accepted hunks (diff-shaped approvals). Omit to
   *  apply the whole patch. */
  acceptedHunks?: number[];
}

export async function approveRun(
  runId: string,
  choice: string,
  opts: ApproveRunOptions = {},
): Promise<void> {
  return invoke("approve_run", {
    args: {
      run_id: runId,
      choice,
      edited_payload: opts.editedPayload ?? null,
      accepted_hunks: opts.acceptedHunks ?? null,
    },
  });
}

export async function stopRun(runId: string): Promise<void> {
  return invoke("stop_run", { args: { run_id: runId } });
}

export async function listAgents(): Promise<AgentDescriptor[]> {
  return invoke<AgentDescriptor[]>("list_agents");
}

export async function checkAgentHealth(agentId: string): Promise<boolean> {
  return invoke<boolean>("check_agent_health", { agentId });
}

export interface GatewayConfig {
  base_url: string;
  model: string;
  has_api_key: boolean;
  ollama_base_url: string;
  ollama_model: string;
  obsidian_vault: string | null;
  git_server_url: string | null;
  git_server_cloned_path: string | null;
}

/** Mirrors the Rust `GitUrlInfo` serde struct (snake_case). */
export interface GitUrlInfo {
  is_valid: boolean;
  normalized_url: string;
  hostname: string;
}

/** Mirrors the Rust `CloneResult` serde struct. On success `project_root` is
 *  the canonical path the repo was registered under — feed it to
 *  `openProjectByPath` for the "Open project" hand-off. */
export interface CloneResult {
  ok: boolean;
  stdout_tail: string;
  stderr_tail: string;
  exit_code: number;
  project_root: string | null;
}

/** Mirrors the Rust `VaultInfo` serde struct. */
export interface VaultInfo {
  path: string;
  is_valid: boolean;
  is_obsidian_vault: boolean;
}

/** Validate a git remote URL (no network / no spawn) for inline UI feedback. */
export async function validateGitUrl(url: string): Promise<GitUrlInfo> {
  return invoke<GitUrlInfo>("validate_git_url", { url });
}

/** Clone a remote repo into `targetDir`. Persists URL + path and registers
 *  the repo as a project on success. */
export async function cloneGitRepo(url: string, targetDir: string): Promise<CloneResult> {
  return invoke<CloneResult>("clone_git_repo", { url, targetDir });
}

/** Inspect a candidate Obsidian vault directory. */
export async function validateObsidianVault(path: string): Promise<VaultInfo> {
  return invoke<VaultInfo>("validate_obsidian_vault", { path });
}

/** Persist the git-server URL without cloning. */
export async function setGitServerUrl(url: string): Promise<void> {
  return invoke("set_git_server_url", { url });
}

/** Connect (and persist) an already-cloned local repo path. Registers it as
 *  a project and returns the canonical path. */
export async function setGitServerClonedPath(path: string): Promise<string> {
  return invoke<string>("set_git_server_cloned_path", { path });
}

export async function getGatewayConfig(): Promise<GatewayConfig> {
  return invoke<GatewayConfig>("get_gateway_config");
}

export async function setGatewayApiKey(apiKey: string): Promise<void> {
  return invoke("set_gateway_api_key", { args: { api_key: apiKey } });
}

export async function updateGatewayConfig(args: {
  gateway_base_url?: string;
  gateway_model?: string;
  ollama_base_url?: string;
  ollama_model?: string;
}): Promise<void> {
  return invoke("update_gateway_config", { args });
}

/** Mirrors the Rust `ProviderConfig` serde struct (snake_case). Key values
 *  never cross the bridge — only their presence is reported. Default-model
 *  slugs are not secrets and do round-trip. */
export interface ProviderConfig {
  anthropic_key_set: boolean;
  openai_key_set: boolean;
  claude_cli_available: boolean;
  runtime_mode: string; // "homelab" | "cloud"
  standalone_build: boolean;
  anthropic_default_model: string | null;
  openai_default_model: string | null;
}

/** Read direct-provider config for the Settings → Providers tab. */
export async function getProviderConfig(): Promise<ProviderConfig> {
  return invoke<ProviderConfig>("get_provider_config");
}

/** Store a direct-provider API key in the OS-backed key vault.
 *  `provider` is "anthropic" | "openai". */
export async function setProviderKey(provider: string, key: string): Promise<void> {
  return invoke("set_provider_key", { args: { provider, key } });
}

/** Outcome of a live provider key check (`validate_provider_key`). */
export interface ProviderValidation {
  ok: boolean;
  message: string;
  /** Live model IDs from the provider's GET /v1/models (empty on failure). */
  models: string[];
}

/** Fire a cheap live round-trip (GET /v1/models) against the saved key.
 *  Failures come back as `ok: false` with a humanized message — the call
 *  only rejects for programmer errors (unknown provider). */
export async function validateProviderKey(provider: string): Promise<ProviderValidation> {
  return invoke<ProviderValidation>("validate_provider_key", { args: { provider } });
}

/** Persist the per-provider default model the direct adapters resolve on
 *  every run. Pass an empty string to clear back to the adapter default. */
export async function setProviderDefaultModel(provider: string, model: string): Promise<void> {
  return invoke("set_provider_default_model", { args: { provider, model } });
}

/** Persist the runtime mode (homelab = Cortex Gateway, cloud = direct
 *  providers) to ~/.cortex/runtime-mode.json. Adapter registration happens
 *  at startup, so the switch applies on the next launch. */
export async function setRuntimeMode(mode: "homelab" | "cloud"): Promise<void> {
  return invoke("set_runtime_mode", { args: { mode } });
}

/** One local AI-maker CLI's detection + sign-in state (Settings → Providers →
 *  "Local AI providers"). Mirrors the Rust `LocalCliProvider` serde struct.
 *  No secret crosses the bridge — only install/auth presence + public strings. */
export interface LocalCliProvider {
  id: string;
  label: string;
  description: string;
  /** Is the CLI binary resolvable on this machine? */
  installed: boolean;
  /** Best-effort sign-in probe: true/false, or null when not file-detectable
   *  (e.g. aider authenticates via env API keys). */
  authenticated: boolean | null;
  install_url: string;
  install_hint: string;
  /** Display string of the login command, e.g. "codex login" ("" if none). */
  login_cmd: string;
  /** True when there's a runnable login flow → show the "Sign in" button. */
  has_login: boolean;
}

/** List every local AI-maker CLI Cortex can drive, with install + sign-in
 *  status. Network-free (filesystem probes only). */
export async function listLocalCliProviders(): Promise<LocalCliProvider[]> {
  return invoke<LocalCliProvider[]>("list_local_cli_providers");
}

/** Launch a CLI provider's own login flow in a PTY so the user can complete it
 *  inside Cortex. Returns a PTY handle id to attach an xterm view to (reuse
 *  `onTerminalOutput`/`writeTerminal`/`closeTerminal` from `@/lib/terminal`).
 *  Rejects when the CLI isn't installed or has no login flow. */
export async function cliProviderLogin(
  providerId: string,
  cols: number,
  rows: number,
): Promise<{ id: string; child_pid: number }> {
  return invoke<{ id: string; child_pid: number }>("cli_provider_login", {
    providerId,
    cols,
    rows,
  });
}

export async function subscribeToSession(
  sessionId: string,
  handler: (env: AgentEventEnvelope) => void,
): Promise<UnlistenFn> {
  return listen<AgentEventEnvelope>(`agent-event:${sessionId}`, (evt) => handler(evt.payload));
}

// ── Ultimate multi-model agent ──────────────────────────────────────────────
// The "Ultimate" agent plans a goal into subtasks, races several connected
// models on each subtask, merges the winners, then synthesizes a final answer.
// `ultimate_chat_run` resolves with the final result; live progress streams as
// `ultimate:event` Tauri events (see {@link UltimateEvent}).

/** One subtask from the planner. Mirrors the Rust serde struct (snake_case). */
export interface UltimateSubtask {
  id: string;
  task: string;
  kind: string;
  difficulty: string;
  fan_out: boolean;
}

/** Resolution value of `ultimate_chat_run`. */
export interface UltimateResult {
  final_output: string;
  subtasks: UltimateSubtask[];
  total_usd: number;
}

/** Tagged-enum payloads streamed on the `ultimate:event` channel. The `type`
 *  discriminant is snake_case to match the Rust serde tag. */
export type UltimateEvent =
  | { type: "plan"; subtasks: UltimateSubtask[] }
  | { type: "subtask_started"; id: string; task: string; models: string[] }
  | { type: "model_done"; subtask_id: string; model: string; ok: boolean; output: string }
  | { type: "subtask_merged"; id: string; merged: string }
  | { type: "synthesis"; merged: string }
  | { type: "cost"; usd: number }
  | { type: "done"; ok: boolean }
  | { type: "error"; msg: string };

export interface UltimateRunArgs {
  goal: string;
  projectRoot?: string | null;
  fanOut?: number | null;
  leadModel?: string | null;
}

/** List the connected-model roster the Ultimate agent can fan out across. */
export async function ultimateListModels(): Promise<string[]> {
  return invoke<string[]>("ultimate_list_models");
}

/** Kick off an Ultimate run. Resolves with the final result once the run
 *  settles; subscribe via {@link subscribeUltimate} for live progress. */
export async function ultimateRun(args: UltimateRunArgs): Promise<UltimateResult> {
  return invoke<UltimateResult>("ultimate_chat_run", {
    goal: args.goal,
    projectRoot: args.projectRoot ?? null,
    fanOut: args.fanOut ?? null,
    leadModel: args.leadModel ?? null,
  });
}

/** Subscribe to the live `ultimate:event` stream. Returns the Tauri unlisten
 *  handle — call it on cleanup. */
export async function subscribeUltimate(
  handler: (event: UltimateEvent) => void,
): Promise<UnlistenFn> {
  return listen<UltimateEvent>("ultimate:event", (evt) => handler(evt.payload));
}

// ── Chat-history import ──────────────────────────────────────────────────────
// Bring external AI chat history (Claude.ai / ChatGPT / generic JSON exports)
// into Cortex as resumable sessions. Mirrors the Rust `ImportResult`
// (src-tauri/src/chat_import/pipeline.rs).

export type ImportProvider = "claude" | "chatgpt";

export interface ImportResult {
  imported: number;
  skipped: number;
  session_ids: string[];
}

/** Import a chat-history export file from disk. Auto-detects the format and
 *  writes each conversation as a resumable + searchable Cortex session. */
export async function importChatFile(path: string): Promise<ImportResult> {
  return invoke<ImportResult>("import_chat_file", { path });
}

/** EXPERIMENTAL: pull chat history live from a provider via a session token.
 *  Unofficial, fragile endpoints — may fail or break. The token is passed once
 *  and never persisted/logged client-side. */
export async function importChatPull(
  provider: ImportProvider,
  token: string,
): Promise<ImportResult> {
  return invoke<ImportResult>("import_chat_pull", { provider, token });
}

/** Cursor-style activation modes for `.cortex/rules/*.md` files. */
export type RuleActivation = "alwaysApply" | "globs" | "description" | "manual";

export interface RuleSummary {
  name: string;
  activation: RuleActivation;
  globs: string[];
  description: string | null;
}

export async function listRules(projectRoot: string): Promise<RuleSummary[]> {
  return invoke<RuleSummary[]>("list_rules", { projectRoot });
}
