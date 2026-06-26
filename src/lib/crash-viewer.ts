import type { CrashRow } from "@/lib/observability";
import { timeAgo as relativeTime } from "@/lib/time";

/**
 * Crash-viewer support utilities. The component itself is summoned via the
 * imperative `openCrashViewer` portal helper (mirrors IDEExportModal /
 * AuditLogPanel). Keeping the helpers in a separate module lets the slash
 * command dynamic-import the modal without dragging in the type plumbing.
 */

/** Subset of CrashRow that we surface in the row's expanded panel. */
export interface CrashDetails {
  id: number;
  ts: number;
  ts_iso: string;
  kind: string;
  message: string;
  stack: string | null;
  /** First line of `stack`, when it looks like a `file:line[:col]` location. */
  file_line: string | null;
  /** Backend writes the CARGO_PKG_VERSION here. */
  version: string | null;
  /** Best-effort UA-derived OS hint — backend doesn't track this today. */
  os: string;
  /** Present only when the originator attached a last_user_message field. */
  last_user_message?: string | null;
}

/** Cheap UA-derived OS label. We never want the full UA string in chat. */
export function detectOs(): string {
  if (typeof navigator === "undefined") return "unknown";
  const ua = navigator.userAgent || "";
  if (/Mac OS X|Macintosh/.test(ua)) return "macOS";
  if (/Windows/.test(ua)) return "Windows";
  if (/Linux/.test(ua)) return "Linux";
  return "unknown";
}

/** Extract `path:line[:col]` from the head of a stack/location string. */
export function firstLocation(stack: string | null): string | null {
  if (!stack) return null;
  const head = stack.split(/\r?\n/, 1)[0]?.trim() ?? "";
  // Common shapes:
  //   src/observability/crash.rs:78:9        (rust panic hook)
  //   at foo (/abs/path/file.ts:12:4)        (JS stack frame)
  //   ChunkLoadError: …                      (no location)
  const m = head.match(/([^\s()]+:\d+(?::\d+)?)/);
  return m ? m[1] : null;
}

/** Build the JSON-friendly view of a row used by "Copy as JSON". */
export function toDetails(row: CrashRow): CrashDetails {
  // Some upstreams stuff extra fields into the row via serde flattening; we
  // surface `last_user_message` if it happens to be there.
  const extra = row as unknown as { last_user_message?: string | null };
  return {
    id: row.id,
    ts: row.ts,
    ts_iso: new Date(row.ts).toISOString(),
    kind: row.kind,
    message: row.message,
    stack: row.stack,
    file_line: firstLocation(row.stack),
    version: row.build_hash,
    os: detectOs(),
    last_user_message:
      typeof extra.last_user_message === "string" ? extra.last_user_message : null,
  };
}

/** Relative, but old crashes (>30d) read as an absolute date. */
export function timeAgo(unixMs: number): string {
  return relativeTime(unixMs, { absoluteAfterDays: 30 });
}

/** Truncate to N chars with an ellipsis. */
export function truncate(s: string, max = 120): string {
  return s.length <= max ? s : `${s.slice(0, max - 1)}…`;
}

/** Severity bucket from the crash kind. Drives the badge colour. */
export type Severity = "fatal" | "error" | "warning";

export function severityOf(kind: string): Severity {
  const k = kind.toLowerCase();
  if (k.includes("panic")) return "fatal";
  if (k.includes("unhandled")) return "error";
  if (k.includes("command")) return "warning";
  return "error";
}

/** Filter categories surfaced as buttons at the top of the modal. */
export const KIND_FILTERS = [
  { id: "all", label: "All" },
  { id: "rust_panic", label: "Rust panic" },
  { id: "js_error", label: "JS error" },
  { id: "js_unhandled_rejection", label: "Unhandled rejection" },
  { id: "tauri_command_error", label: "Tauri command failure" },
] as const;

export type CrashKindFilter = (typeof KIND_FILTERS)[number]["id"];

/** Dispatch the chat-replay event consumed by ChatPane. */
export function dispatchReplay(message: string): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent("cortex:chat-replay", { detail: { message } }));
}

/**
 * Imperative summoner — mounts the modal on a detached root attached to
 * document.body. Lives here (not in the .tsx) so callers can `await import`
 * just this helper and let the component module load lazily inside.
 */
export async function openCrashViewer(): Promise<void> {
  const { mountCrashViewer } = await import("@/components/CrashViewer");
  mountCrashViewer();
}
