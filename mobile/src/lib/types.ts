// Wire types for the Cortex mobile API (../mobile-contract.md, `/api/v2/*`)
// plus the legacy `/api/*` shapes the client falls back to when the server
// predates v2. Everything is kept defensive: the server may add fields, and
// we only rely on what the contract documents.

// ── Capabilities / mode ────────────────────────────────────────────────────

export type Feature =
  | "threads"
  | "replay"
  | "routines"
  | "git"
  | "checkpoints"
  | "reliability"
  | "usage"
  | "projects.add"
  | string;

export interface Capabilities {
  server_version: string;
  features: Feature[];
  local_agents: string[];
  gateway: boolean;
}

/** v2 = contract server; legacy = today's `/api/*` PWA endpoints only. */
export type ApiMode = "v2" | "legacy";

export interface Health {
  ok: boolean;
  version: string;
}

// ── Auth ───────────────────────────────────────────────────────────────────

export interface PairResponse {
  token: string;
  device_id: string;
  server_name: string;
  server_version: string;
}

export interface Device {
  id: string;
  name: string;
  created_ms?: number;
  last_seen_ms?: number;
  current?: boolean;
}

// ── Threads / messages ─────────────────────────────────────────────────────

export interface Thread {
  id: string;
  title: string;
  project_root?: string | null;
  agent_id?: string | null;
  model?: string | null;
  created_ms?: number;
  last_ms: number;
  pending_approvals?: number;
  running?: boolean;
  last_preview?: string;
}

export type ToolStatus = "pending" | "approved" | "denied" | "done" | "error";

export interface ToolCall {
  id: string;
  name: string;
  args_preview?: string;
  status: ToolStatus;
  result_preview?: string;
  duration_ms?: number;
  /** Client-only: when the call was first seen (for a live duration). */
  started_ms?: number;
}

/** Server-provided risk hint for an approval. */
export type Risk = "read" | "write" | "exec" | "network" | string;

export interface Approval {
  id: string;
  tool?: string | null;
  detail?: string | null;
  resolved?: boolean;
  risk?: Risk | null;
  /** Legacy `/api/approvals` rows carry run_id + choices instead of detail. */
  run_id?: string;
  thread_id?: string;
  preview?: string | null;
  choices?: string[];
}

export type Role = "user" | "assistant" | "system";

export interface Message {
  id: string;
  role: Role;
  content: string;
  ts_ms: number;
  run_id?: string;
  reasoning?: string;
  tool_calls?: ToolCall[];
  approval?: Approval;
  error?: string;
  routing_reason?: string;
  usage?: Usage;
  /** Client-only: still receiving tokens. */
  streaming?: boolean;
  /** Client-only: queued in the outbox, not yet accepted by the server. */
  queued?: boolean;
  attachments?: Attachment[];
}

export interface Usage {
  input_tokens?: number;
  output_tokens?: number;
  total_tokens?: number;
  cost_usd?: number;
}

export interface Attachment {
  name: string;
  mime: string;
  data_base64: string;
}

export interface SendBody {
  content: string;
  model?: string;
  agent_id?: string;
  attachments?: Attachment[];
}

// ── Projects / git ─────────────────────────────────────────────────────────

export interface Project {
  root: string;
  name: string;
  trusted?: boolean;
  branch?: string | null;
  dirty_files?: number | null;
  last_opened_ms?: number | null;
  /** Legacy `/api/projects` extras. */
  group?: string;
  kind?: string;
  subtitle?: string | null;
}

export interface GitFile {
  path: string;
  status: string;
}

export interface GitStatus {
  branch?: string | null;
  ahead?: number;
  behind?: number;
  files: GitFile[];
}

export interface GitDiff {
  diff: string;
  truncated: boolean;
}

export interface Checkpoint {
  id: string;
  label?: string;
  created_ms?: number;
  ts_ms?: number;
  files?: number;
  [k: string]: unknown;
}

// ── Runs / replay / observability ──────────────────────────────────────────

export type RunStatus = "running" | "done" | "error" | "stopped";

export interface Run {
  run_id: string;
  thread_id?: string;
  started_ms: number;
  ended_ms?: number | null;
  status: RunStatus;
  agent_id?: string | null;
  model?: string | null;
  cost_usd?: number | null;
  tokens?: number | null;
}

export type TimelineKind =
  | "prompt"
  | "route"
  | "tool_call"
  | "tool_result"
  | "approval"
  | "edit"
  | "error"
  | "result"
  | string;

export interface TimelineEvent {
  ts_ms: number;
  kind: TimelineKind;
  summary: string;
  detail?: string | null;
}

export interface ReliabilityRow {
  key?: string;
  agent_id?: string | null;
  model?: string | null;
  runs?: number;
  ok_runs?: number;
  error_runs?: number;
  success_rate?: number;
  p50_ms?: number | null;
  p95_ms?: number | null;
  total_tokens?: number;
  tokens?: number;
  est_usd?: number;
  cost_usd?: number;
  top_error_class?: string | null;
}

export interface ReliabilityReport {
  totals?: ReliabilityRow;
  by_provider?: ReliabilityRow[];
  by_model?: ReliabilityRow[];
  generated_ms?: number;
}

export interface QuotaWindow {
  five_hour_pct?: number | null;
  seven_day_pct?: number | null;
  resets_ms?: number | null;
}

export interface UsageReport {
  claude?: QuotaWindow | null;
  chatgpt?: QuotaWindow | null;
  budget?: { spent_usd?: number; cap_usd?: number | null } | null;
}

// ── Routines ───────────────────────────────────────────────────────────────

export interface Routine {
  id: string;
  name: string;
  prompt: string;
  interval_minutes: number;
  enabled: boolean;
  last_run_unix_ms?: number;
  last_status?: string;
  last_output?: string;
  last_error?: string;
  agent_id?: string | null;
  project_root?: string | null;
  daily_at?: string | null;
  next_run_unix_ms?: number | null;
}

export interface RoutineHistoryRow {
  ts_ms?: number;
  started_ms?: number;
  status?: string;
  output?: string;
  error?: string;
  [k: string]: unknown;
}

// ── Models / settings / push ───────────────────────────────────────────────

export interface Model {
  id: string;
  label: string;
  provider?: string;
  capabilities?: string[];
  local?: boolean;
  cost_tier?: "free" | "low" | "mid" | "high" | string;
}

export interface MobileSettings {
  default_model?: string | null;
  default_agent_id?: string | null;
  plan_mode?: boolean;
  sandbox_tier?: string | null;
}

export interface PushStatus {
  provider?: string | null;
  enabled?: boolean;
  events?: string[];
  [k: string]: unknown;
}

// ── Legacy shapes (`/api/*`) ───────────────────────────────────────────────

export interface LegacySession {
  id: string;
  title: string;
  last_ts: number;
  message_count: number;
  preview: string;
}

export interface LegacyStoredMessage {
  id: string;
  session_id: string;
  ts: number;
  role: string;
  content: string;
  run_id?: string | null;
  reasoning?: string | null;
}

export interface LegacyProject {
  root?: string;
  path?: string;
  name?: string;
  group?: string;
  kind?: string;
  subtitle?: string | null;
  has_git?: boolean;
}

// ── WebSocket ──────────────────────────────────────────────────────────────

/** Raw frame: `{ type, ... }` from either the v2 or the legacy server. */
export interface WsFrame {
  type: string;
  [k: string]: unknown;
}

/** Normalised stream event. Both wire dialects (v2 `token`, legacy
 *  `chat_token`, …) are mapped onto this by `ws.ts` so views only handle one
 *  vocabulary. */
export type StreamEvent =
  | { type: "token"; run_id: string; thread_id?: string; delta: string }
  | { type: "reasoning"; run_id: string; thread_id?: string; delta: string }
  | { type: "tool_call"; run_id: string; thread_id?: string; tool: ToolCall }
  | {
      type: "tool_result";
      run_id: string;
      thread_id?: string;
      tool: Partial<ToolCall> & { name?: string };
    }
  | {
      type: "approval_request";
      run_id: string;
      thread_id?: string;
      approval: Approval;
    }
  | {
      type: "approval_resolved";
      run_id?: string;
      thread_id?: string;
      approval_id?: string;
      decision: string;
    }
  | { type: "done"; run_id: string; thread_id?: string; usage?: Usage }
  | { type: "error"; run_id: string; thread_id?: string; message: string }
  | { type: "thread_updated"; thread: Thread }
  | { type: "ping" }
  | { type: "other"; frame: WsFrame };

/** Path helpers shared by list rows. The desktop host may be Windows, so
 *  accept either separator. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}
