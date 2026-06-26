// Wire types mirroring the Cortex embedded mobile server
// (src-tauri/src/mobile_server). Kept intentionally defensive — the server may
// add fields, and we only depend on the ones documented in the API contract.

export interface Health {
  ok: boolean;
  version: string;
}

// GET /api/projects → ProjectMeta[] (be defensive: only `name` + a path are
// guaranteed-useful; the rest are best-effort hints).
export interface Project {
  root?: string;
  name?: string;
  group?: string;
  kind?: string;
  subtitle?: string | null;
  has_git?: boolean;
  [k: string]: unknown;
}

/** Best-effort path for a project row (server uses `root`). */
export function projectPath(p: Project): string {
  return (p.root as string) || (p.path as string) || "";
}

/** Best-effort display name. */
export function projectName(p: Project): string {
  if (p.name) return p.name;
  const path = projectPath(p);
  return path.split("/").filter(Boolean).pop() || path || "(unnamed)";
}

// GET /api/sessions → recent chat sessions (newest first).
export interface SessionSummary {
  id: string;
  title: string;
  last_ts: number;
  message_count: number;
  preview: string;
}

// GET /api/sessions/{id}/messages → one session's full history (oldest first).
// Mirrors `StoredMessage` in tracing_store.rs.
export interface StoredMessage {
  id: string;
  session_id: string;
  ts: number;
  role: string;
  agent_id?: string | null;
  content: string;
  run_id?: string | null;
  reasoning?: string | null;
  project_root?: string | null;
}

// POST /api/import/file | /api/import/pull → import outcome. Mirrors the Rust
// `ImportResult` (src-tauri/src/chat_import/pipeline.rs).
export interface ImportResult {
  imported: number;
  skipped: number;
  session_ids: string[];
}

export type ImportFormat = "auto" | "claude" | "chatgpt" | "generic";
export type ImportProvider = "claude" | "chatgpt";

export interface Approval {
  id: string;
  run_id: string;
  tool?: string | null;
  preview?: string | null;
  choices: string[];
  request?: unknown;
}

// ── WebSocket frames ────────────────────────────────────────────────────────
// Each frame is `{ type, run_id, ... }`. We model the discriminated union loosely
// and narrow on `type` at the call site.

export interface WsFrameBase {
  type: string;
  run_id?: string;
  [k: string]: unknown;
}

// Ultimate sub-events live under `ultimate.event.type`.
export interface UltEvent {
  type:
    | "plan"
    | "subtask_started"
    | "model_done"
    | "subtask_merged"
    | "synthesis"
    | "cost"
    | "done"
    | "error"
    | string;
  [k: string]: unknown;
}

export interface PlannedSubtask {
  id: string;
  task: string;
  kind: string;
  difficulty: string;
  fan_out: boolean;
}
