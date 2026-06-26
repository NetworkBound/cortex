import { invoke } from "@tauri-apps/api/core";

export interface Span {
  id: string;
  parent_id: string | null;
  trace_id: string;
  session_id: string;
  agent_id: string | null;
  name: string;
  started_at: number;
  ended_at: number | null;
  status: "running" | "ok" | "error";
  attributes: Record<string, unknown>;
}

export interface Trace {
  trace_id: string;
  session_id: string;
  started_at: number;
  spans: Span[];
}

export interface HealthRow {
  source: string;
  ts: number;
  ok: boolean;
  latency_ms: number | null;
}

export interface IssueRow {
  fingerprint: string;
  agent_id: string | null;
  error_class: string | null;
  message: string;
  first_seen: number;
  last_seen: number;
  count: number;
}

export interface AuditRow {
  ts: number;
  session_id: string | null;
  agent_id: string | null;
  action: string;
  detail: string | null;
}

export interface TraceEvent {
  ts: number;
  name: string;
  span_name: string;
  agent_id: string | null;
  payload: unknown;
}

export interface CrashRow {
  id: number;
  ts: number;
  kind: string;
  message: string;
  stack: string | null;
  build_hash: string | null;
}

export async function recentTraces(limit = 20): Promise<Trace[]> {
  return invoke<Trace[]>("recent_traces", { limit });
}

export async function traceEvents(traceId: string): Promise<TraceEvent[]> {
  return invoke<TraceEvent[]>("trace_events", { traceId });
}

export async function recentIssues(limit = 50): Promise<IssueRow[]> {
  return invoke<IssueRow[]>("recent_issues", { limit });
}

export async function recentAudit(limit = 100): Promise<AuditRow[]> {
  return invoke<AuditRow[]>("recent_audit", { limit });
}

export async function recentCrashes(limit = 50): Promise<CrashRow[]> {
  return invoke<CrashRow[]>("recent_crashes", { limit });
}

export async function recordJsCrash(kind: "js_error" | "js_unhandled_rejection", message: string, stack?: string): Promise<void> {
  await invoke<void>("record_js_crash", { kind, message, stack: stack ?? null });
}

export async function homelabHealth(): Promise<HealthRow[]> {
  return invoke<HealthRow[]>("homelab_health");
}
