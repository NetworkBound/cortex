// Centralised error→message humaniser.
//
// Panels and toasts used to render `String(e)` directly, leaking raw JS
// exception dumps to the user ("TypeError: Cannot read properties of undefined
// (reading 'invoke')", "Error: …"). That's the clearest "not professional"
// tell on an otherwise polished surface. Every UI error sink now routes the
// caught value through `humanizeError`, which:
//   • strips the noisy `TypeError:` / `Error:` constructor prefix,
//   • maps known low-level failures (an unreachable Tauri backend, dropped
//     network calls) onto friendly, actionable copy,
//   • degrades gracefully for non-Error throws (strings, plain objects).
//
// Keep this provider-agnostic and dependency-free — it is imported widely.

function rawMessage(e: unknown): string {
  if (e == null) return "";
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message || e.name || String(e);
  if (typeof e === "object") {
    const o = e as Record<string, unknown>;
    if (typeof o.message === "string") return o.message;
    if (typeof o.error === "string") return o.error;
    try {
      return JSON.stringify(e);
    } catch {
      return String(e);
    }
  }
  return String(e);
}

/**
 * Convert any thrown value into a clean, human-readable sentence suitable for
 * display in a panel error state or a toast body.
 */
export function humanizeError(e: unknown): string {
  const raw = rawMessage(e).trim();

  // The Tauri IPC bridge is missing or not ready — the desktop backend can't
  // be reached (app still starting, opened outside the shell, backend crash).
  if (/reading '?invoke'?|__TAURI|isTauri|window\.__TAURI/i.test(raw)) {
    return "Cortex's backend isn't responding right now. Try reopening the app.";
  }

  // Network / gateway reachability failures from fetch-based calls.
  if (/failed to fetch|networkerror|err_connection|econnrefused|fetch failed|timed out|timeout/i.test(raw)) {
    return "Couldn't reach the gateway — check your connection and try again.";
  }

  // Strip a leading JS error-constructor prefix ("TypeError: ", "Error: ", …)
  // so the meaningful part of a real error reads as a plain sentence.
  const cleaned = raw.replace(/^[A-Za-z]*Error:\s*/, "").trim();
  return cleaned || "Something went wrong.";
}
