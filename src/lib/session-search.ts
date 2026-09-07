import { invoke } from "@tauri-apps/api/core";

export interface SessionSearchHit {
  session_id: string;
  ts: number;
  role: string;
  snippet: string;
}

/**
 * Full-text-LIKE search across stored chat messages.
 * Empty / whitespace-only queries are short-circuited to `[]` without round-tripping.
 */
export async function searchSessions(
  query: string,
  limit?: number,
): Promise<SessionSearchHit[]> {
  const q = query.trim();
  if (!q) return [];
  return invoke<SessionSearchHit[]>("search_sessions", { query: q, limit });
}
