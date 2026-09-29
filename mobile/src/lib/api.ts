// API surface used by the views. Every call goes to `/api/v2/*` when the
// server advertised it (GET /api/v2/capabilities succeeded) and falls back to
// the legacy `/api/*` routes the original PWA used otherwise. Views never
// branch on the mode themselves except to hide features that have no legacy
// equivalent (`hasFeature`).

import { del, get, patch, post, put, q, request } from "./http";
import type {
  ApiMode,
  Approval,
  Capabilities,
  Checkpoint,
  Device,
  GitDiff,
  GitStatus,
  Health,
  LegacyProject,
  LegacySession,
  LegacyStoredMessage,
  Message,
  MobileSettings,
  Model,
  PairResponse,
  Project,
  PushStatus,
  ReliabilityReport,
  Routine,
  RoutineHistoryRow,
  Run,
  SendBody,
  Thread,
  TimelineEvent,
  UsageReport,
} from "./types";
import { baseName } from "./types";

let mode: ApiMode = "v2";
let caps: Capabilities | null = null;

export function setMode(m: ApiMode, c: Capabilities | null) {
  mode = m;
  caps = c;
}
export const apiMode = () => mode;
export const capabilities = () => caps;
export const isLegacy = () => mode === "legacy";

/** Feature gate. Legacy servers only have chat + approvals + projects. */
export function hasFeature(f: string): boolean {
  if (mode === "legacy") return f === "threads" || f === "approvals";
  if (!caps || !Array.isArray(caps.features) || caps.features.length === 0) {
    return true; // contract server without a features list: assume all
  }
  return caps.features.includes(f);
}

// ── Bootstrap ──────────────────────────────────────────────────────────────

export const getHealth = () => get<Health>("/api/health", 6_000);

/** `null` when the server has no v2 (legacy mode). Throws on 401/network. */
export const getCapabilities = () =>
  request<Capabilities | null>("/api/v2/capabilities", {
    allow404: true,
    timeoutMs: 8_000,
  });

export const pair = (code: string, device_name: string) =>
  post<PairResponse>("/api/v2/pair", { code, device_name });

// ── Threads ────────────────────────────────────────────────────────────────

export async function listThreads(
  project?: string,
  cursor?: string,
): Promise<{ threads: Thread[]; next_cursor?: string | null }> {
  if (mode === "legacy") {
    const rows = await get<LegacySession[]>(`/api/sessions${q({ limit: 50 })}`);
    return {
      threads: (Array.isArray(rows) ? rows : []).map((s) => ({
        id: s.id,
        title: s.title || "New chat",
        last_ms: s.last_ts,
        last_preview: s.preview,
      })),
      next_cursor: null,
    };
  }
  const r = await get<{ threads: Thread[]; next_cursor?: string | null }>(
    `/api/v2/threads${q({ project, limit: 50, cursor })}`,
  );
  return { threads: r?.threads ?? [], next_cursor: r?.next_cursor ?? null };
}

export const createThread = (project_root?: string, title?: string) =>
  post<Thread>("/api/v2/threads", { project_root, title });

export const renameThread = (id: string, title: string) =>
  patch<Thread>(`/api/v2/threads/${encodeURIComponent(id)}`, { title });

export const deleteThread = (id: string) =>
  del<void>(`/api/v2/threads/${encodeURIComponent(id)}`);

/** `after` = resync everything newer than a message id (foreground /
 *  reconnect); servers without it just return the page and we merge by id. */
export async function getMessages(
  threadId: string,
  opts: { before?: string; after?: string } = {},
): Promise<Message[]> {
  if (mode === "legacy") {
    const rows = await get<LegacyStoredMessage[]>(
      `/api/sessions/${encodeURIComponent(threadId)}/messages`,
    );
    return (Array.isArray(rows) ? rows : [])
      .filter((m) => m.role === "user" || m.role === "assistant")
      .map((m) => ({
        id: m.id,
        role: m.role === "user" ? "user" : "assistant",
        content: m.content,
        ts_ms: m.ts,
        run_id: m.run_id ?? undefined,
        reasoning: m.reasoning ?? undefined,
      }));
  }
  const r = await get<{ messages: Message[] }>(
    `/api/v2/threads/${encodeURIComponent(threadId)}/messages${q({ limit: 200, before: opts.before, after: opts.after })}`,
  );
  return r?.messages ?? [];
}

/** Send a message. In legacy mode `threadId` may be undefined (new session);
 *  the returned `thread_id` is then the session the server created. */
export async function sendMessage(
  threadId: string | undefined,
  body: SendBody & { project_root?: string },
): Promise<{ run_id: string; thread_id: string }> {
  if (mode === "legacy") {
    const r = await post<{ run_id: string; session_id: string }>(
      "/api/chat",
      {
        session_id: threadId,
        message: body.content,
        model: body.model,
        project_root: body.project_root,
      },
      30_000,
    );
    return { run_id: r.run_id, thread_id: r.session_id };
  }
  if (!threadId) throw new Error("no thread");
  const r = await post<{ run_id: string }>(
    `/api/v2/threads/${encodeURIComponent(threadId)}/send`,
    {
      content: body.content,
      model: body.model,
      agent_id: body.agent_id,
      attachments: body.attachments,
    },
    60_000,
  );
  return { run_id: r.run_id, thread_id: threadId };
}

export const stopRun = (runId: string) =>
  post<void>(`/api/v2/runs/${encodeURIComponent(runId)}/stop`);

// ── Approvals ──────────────────────────────────────────────────────────────

/** Pending approvals across all runs. The legacy list route is the only one
 *  the contract keeps, so it is used in both modes. */
export async function listApprovals(): Promise<Approval[]> {
  const rows = await get<Approval[]>("/api/approvals");
  return Array.isArray(rows) ? rows : [];
}

export async function resolveApproval(
  id: string,
  decision: "approve" | "deny",
  remember?: boolean,
): Promise<void> {
  if (mode === "legacy") {
    await post<void>(`/api/approvals/${encodeURIComponent(id)}`, {
      approve: decision === "approve",
    });
    return;
  }
  await post<void>(`/api/v2/approvals/${encodeURIComponent(id)}`, {
    decision,
    remember,
  });
}

// ── Projects / git / checkpoints ───────────────────────────────────────────

export async function listProjects(): Promise<Project[]> {
  if (mode === "legacy") {
    const rows = await get<LegacyProject[]>("/api/projects");
    return (Array.isArray(rows) ? rows : [])
      .map((p) => {
        const root = p.root || p.path || "";
        return {
          root,
          name: p.name || baseName(root) || "(unnamed)",
          group: p.group,
          kind: p.kind,
          subtitle: p.subtitle,
        };
      })
      .filter((p) => p.root);
  }
  const r = await get<{ projects: Project[] }>("/api/v2/projects");
  return (r?.projects ?? []).map((p) => ({
    ...p,
    name: p.name || baseName(p.root),
  }));
}

export async function discoverProjects(): Promise<Project[]> {
  const r = await get<{ projects?: Project[]; roots?: string[] } | Project[]>(
    "/api/v2/projects/discover",
  );
  const list = Array.isArray(r) ? r : (r?.projects ?? []);
  const roots = !Array.isArray(r) && r?.roots ? r.roots : [];
  return [
    ...list.map((p) => ({ ...p, name: p.name || baseName(p.root) })),
    ...roots.map((root) => ({ root, name: baseName(root) })),
  ];
}

export const addProject = (root: string) =>
  post<Project>("/api/v2/projects/add", { root });

export const gitStatus = (root: string) =>
  get<GitStatus>(`/api/v2/projects/git/status${q({ root })}`);

export const gitDiff = (root: string, path: string) =>
  get<GitDiff>(`/api/v2/projects/git/diff${q({ root, path })}`);

export async function listCheckpoints(root: string): Promise<Checkpoint[]> {
  const r = await get<{ checkpoints?: Checkpoint[] } | Checkpoint[]>(
    `/api/v2/checkpoints${q({ root })}`,
  );
  return Array.isArray(r) ? r : (r?.checkpoints ?? []);
}

export const createCheckpoint = (root: string, label: string) =>
  post<Checkpoint>("/api/v2/checkpoints", { root, label }, 60_000);

export const restoreCheckpoint = (id: string) =>
  request<void>(`/api/v2/checkpoints/${encodeURIComponent(id)}/restore`, {
    method: "POST",
    body: {},
    headers: { "X-Confirm": "restore" },
    timeoutMs: 60_000,
  });

// ── Runs / observability ───────────────────────────────────────────────────

export async function listRuns(threadId?: string, limit = 50): Promise<Run[]> {
  const r = await get<{ runs: Run[] }>(
    `/api/v2/runs${q({ limit, thread_id: threadId })}`,
  );
  return r?.runs ?? [];
}

export async function runTimeline(runId: string): Promise<TimelineEvent[]> {
  const r = await get<{ events: TimelineEvent[] }>(
    `/api/v2/runs/${encodeURIComponent(runId)}/timeline`,
  );
  return r?.events ?? [];
}

export const reliability = (range = "7d") =>
  get<ReliabilityReport>(`/api/v2/reliability${q({ range })}`);

export const usage = () => get<UsageReport>("/api/v2/usage");

// ── Routines ───────────────────────────────────────────────────────────────

export async function listRoutines(): Promise<Routine[]> {
  const r = await get<{ routines?: Routine[] } | Routine[]>("/api/v2/routines");
  return Array.isArray(r) ? r : (r?.routines ?? []);
}

export const createRoutine = (spec: Partial<Routine>) =>
  post<Routine>("/api/v2/routines", spec);

export const updateRoutine = (id: string, spec: Partial<Routine>) =>
  patch<Routine>(`/api/v2/routines/${encodeURIComponent(id)}`, spec);

export const deleteRoutine = (id: string) =>
  del<void>(`/api/v2/routines/${encodeURIComponent(id)}`);

export const runRoutine = (id: string) =>
  post<void>(`/api/v2/routines/${encodeURIComponent(id)}/run`, {}, 30_000);

export async function routineHistory(id: string): Promise<RoutineHistoryRow[]> {
  const r = await get<
    | { history?: RoutineHistoryRow[]; runs?: RoutineHistoryRow[] }
    | RoutineHistoryRow[]
  >(`/api/v2/routines/${encodeURIComponent(id)}/history`);
  return Array.isArray(r) ? r : (r?.history ?? r?.runs ?? []);
}

// ── Models / settings / push / devices ─────────────────────────────────────

export async function listModels(): Promise<{
  models: Model[];
  default?: string;
}> {
  if (mode === "legacy") {
    const rows = await get<string[]>("/api/models");
    return {
      models: (Array.isArray(rows) ? rows : []).map((id) => ({
        id,
        label: id,
      })),
    };
  }
  const r = await get<{ models: Model[]; default?: string }>("/api/v2/models");
  return {
    models: (r?.models ?? []).map((m) => ({ ...m, label: m.label || m.id })),
    default: r?.default,
  };
}

export const getSettings = () => get<MobileSettings>("/api/v2/settings/mobile");
export const putSettings = (s: MobileSettings) =>
  put<MobileSettings>("/api/v2/settings/mobile", s);

export const pushStatus = () => get<PushStatus>("/api/v2/push/status");

export async function listDevices(): Promise<Device[]> {
  const r = await get<{ devices?: Device[] } | Device[]>("/api/v2/devices");
  return Array.isArray(r) ? r : (r?.devices ?? []);
}

export const revokeDevice = (id: string) =>
  del<void>(`/api/v2/devices/${encodeURIComponent(id)}`);

// ── Legacy-only extras kept reachable from More ────────────────────────────

export interface ImportResult {
  imported: number;
  skipped: number;
  session_ids: string[];
}

export const importChatFile = (content: string) =>
  post<ImportResult>("/api/import/file", { content, format: "auto" }, 120_000);
