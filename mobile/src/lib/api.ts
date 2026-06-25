// Same-origin API client. The Cortex embedded server hosts both this SPA and
// the API at one origin, so paths are relative. (In dev, vite proxies `/api`
// and `/ws` to VITE_API_BASE — see vite.config.ts.)

import type { Approval, Health, Project } from "./types";

async function jsonOrThrow<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let detail = "";
    try {
      detail = JSON.stringify(await res.json());
    } catch {
      /* ignore */
    }
    throw new Error(`${res.status} ${res.statusText}${detail ? ` — ${detail}` : ""}`);
  }
  return res.json() as Promise<T>;
}

export async function getHealth(): Promise<Health> {
  return jsonOrThrow<Health>(await fetch("/api/health"));
}

export async function getProjects(): Promise<Project[]> {
  return jsonOrThrow<Project[]>(await fetch("/api/projects"));
}

export async function getModels(): Promise<string[]> {
  return jsonOrThrow<string[]>(await fetch("/api/models"));
}

export interface ChatStartResponse {
  run_id: string;
  session_id: string;
}

export interface ChatBody {
  session_id?: string;
  message: string;
  model?: string;
  project_root?: string;
}

export async function postChat(body: ChatBody): Promise<ChatStartResponse> {
  return jsonOrThrow<ChatStartResponse>(
    await fetch("/api/chat", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

export interface UltimateBody {
  goal: string;
  project_root?: string;
  fan_out?: number;
  lead_model?: string;
}

export interface UltimateResult {
  final_output?: string;
  subtasks?: unknown[];
  total_usd?: number;
  [k: string]: unknown;
}

export interface UltimateResponse {
  run_id: string;
  result: UltimateResult;
}

export async function postUltimate(body: UltimateBody): Promise<UltimateResponse> {
  return jsonOrThrow<UltimateResponse>(
    await fetch("/api/ultimate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

export async function getApprovals(): Promise<Approval[]> {
  return jsonOrThrow<Approval[]>(await fetch("/api/approvals"));
}

export async function resolveApproval(
  id: string,
  approve: boolean,
  reason?: string,
): Promise<void> {
  const res = await fetch(`/api/approvals/${encodeURIComponent(id)}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ approve, reason }),
  });
  if (!res.ok) {
    throw new Error(`approval ${id} failed: ${res.status} ${res.statusText}`);
  }
}
