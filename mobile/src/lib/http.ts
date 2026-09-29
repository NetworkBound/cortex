// Fetch wrapper: base URL (same-origin for the PWA, the paired server URL in
// the native shell), bearer auth, per-request timeouts and the contract's
// `{ error: { code, message } }` envelope surfaced as `ApiError`.

import { session } from "./session";
import { demo } from "./demo";

export type ErrorCode =
  | "not_found"
  | "unauthorized"
  | "invalid"
  | "unavailable"
  | "internal"
  | "timeout"
  | "network"
  | string;

export class ApiError extends Error {
  code: ErrorCode;
  status: number;
  constructor(code: ErrorCode, message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
  }
}

export function isApiError(e: unknown, code?: ErrorCode): e is ApiError {
  return e instanceof ApiError && (code === undefined || e.code === code);
}

export function errorMessage(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export const DEFAULT_TIMEOUT_MS = 15_000;

/** Absolute URL for an API path. Relative when served same-origin. */
export function apiUrl(path: string): string {
  const base = session.baseUrl();
  if (!base) return path;
  return base.replace(/\/+$/, "") + path;
}

export function wsUrl(): string {
  const base = session.baseUrl();
  const token = session.token();
  let url: string;
  if (base) {
    url = base.replace(/^http/, "ws").replace(/\/+$/, "") + "/ws";
  } else {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    url = `${proto}://${location.host}/ws`;
  }
  if (token) url += `?token=${encodeURIComponent(token)}`;
  return url;
}

interface ReqInit {
  method?: string;
  body?: unknown;
  headers?: Record<string, string>;
  timeoutMs?: number;
  /** Don't treat a 404 as an error (feature probing). */
  allow404?: boolean;
}

/** JSON request with timeout + auth. Resolves the parsed body, or `null` for
 *  an empty 2xx / an allowed 404. */
export async function request<T>(path: string, init: ReqInit = {}): Promise<T> {
  if (demo.active) {
    return demo.request<T>(
      init.method ?? (init.body !== undefined ? "POST" : "GET"),
      path,
      init.body,
    );
  }
  const ctrl = new AbortController();
  const timer = setTimeout(
    () => ctrl.abort(),
    init.timeoutMs ?? DEFAULT_TIMEOUT_MS,
  );
  const headers: Record<string, string> = { ...(init.headers ?? {}) };
  if (init.body !== undefined) headers["content-type"] = "application/json";
  const token = session.token();
  if (token) headers["authorization"] = `Bearer ${token}`;

  let res: Response;
  try {
    res = await fetch(apiUrl(path), {
      method: init.method ?? (init.body !== undefined ? "POST" : "GET"),
      headers,
      body: init.body !== undefined ? JSON.stringify(init.body) : undefined,
      signal: ctrl.signal,
      credentials: "omit",
    });
  } catch (e) {
    clearTimeout(timer);
    if (ctrl.signal.aborted) {
      throw new ApiError("timeout", "The server took too long to respond.", 0);
    }
    throw new ApiError(
      "network",
      e instanceof Error && e.message ? "Can't reach the server." : String(e),
      0,
    );
  }
  clearTimeout(timer);

  if (res.status === 404 && init.allow404) return null as T;
  const text = await res.text();
  let json: unknown = null;
  if (text) {
    try {
      json = JSON.parse(text);
    } catch {
      json = null;
    }
  }
  if (!res.ok) {
    const env = json as { error?: { code?: string; message?: string } } | null;
    const code =
      env?.error?.code ??
      (res.status === 401
        ? "unauthorized"
        : res.status === 404
          ? "not_found"
          : res.status >= 500
            ? "internal"
            : "invalid");
    const msg =
      env?.error?.message ??
      (typeof json === "object" && json && "error" in json
        ? String((json as { error: unknown }).error)
        : text.slice(0, 200) || `${res.status} ${res.statusText}`);
    if (res.status === 401) session.notifyUnauthorized();
    throw new ApiError(code, msg, res.status);
  }
  return json as T;
}

export const get = <T>(path: string, timeoutMs?: number) =>
  request<T>(path, { timeoutMs });
export const post = <T>(path: string, body: unknown = {}, timeoutMs?: number) =>
  request<T>(path, { method: "POST", body, timeoutMs });
export const patch = <T>(path: string, body: unknown) =>
  request<T>(path, { method: "PATCH", body });
export const put = <T>(path: string, body: unknown) =>
  request<T>(path, { method: "PUT", body });
export const del = <T>(path: string) => request<T>(path, { method: "DELETE" });

export const q = (params: Record<string, string | number | undefined>) => {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === "") continue;
    parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  }
  return parts.length ? `?${parts.join("&")}` : "";
};
