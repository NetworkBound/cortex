import type { Message } from "@/state/store";

/** What a finished assistant turn did, for the "Done — …" footer. */
export interface RunSummary {
  /** Files edited (from `file_edit` notes or successful write/edit tools). */
  edits: number;
  /** Tool calls made in this turn. */
  tools: number;
  /** Tool calls that returned an error. */
  toolErrors: number;
}

// Same shape ChatPane uses to decide an auto-checkpoint: a tool whose name
// says it mutates the workspace.
const WRITE_TOOL_RE = /(write|edit|patch|apply_patch|str_replace|create_file)/i;
// ChatPane appends `_edited <path> (<n> lines)_` to the transcript for every
// `file_edit` event — count those lines as edits.
const FILE_EDIT_NOTE_RE = /^_edited .+ \(\d+ lines\)_$/gm;

/** Pure: derive the footer counts from one message. */
export function summarizeRun(m: Message): RunSummary {
  const noteEdits = (m.content.match(FILE_EDIT_NOTE_RE) ?? []).length;
  let toolEdits = 0;
  let toolErrors = 0;
  for (const t of m.tools) {
    if (t.status === "error") toolErrors++;
    if (t.status === "ok" && WRITE_TOOL_RE.test(t.name)) toolEdits++;
  }
  // An agent may report an edit both ways (tool_result + file_edit); take the
  // larger count rather than the sum so we never double-count.
  return {
    edits: Math.max(noteEdits, toolEdits),
    tools: m.tools.length,
    toolErrors,
  };
}

/** Short human label: "3 edits · 7 tools · 1 failed". */
export function describeRun(s: RunSummary): string {
  const parts: string[] = [];
  if (s.edits > 0) parts.push(`${s.edits} edit${s.edits === 1 ? "" : "s"}`);
  parts.push(`${s.tools} tool${s.tools === 1 ? "" : "s"}`);
  if (s.toolErrors > 0) parts.push(`${s.toolErrors} failed`);
  return parts.join(" · ");
}

/**
 * Id of the latest COMPLETED assistant turn that has a run id — the one that
 * gets the "Done — …" footer. Walks back from the end; a still-streaming
 * assistant turn yields null so the footer waits for the run to finish rather
 * than decorating the previous turn mid-stream.
 */
export function latestFinishedRunMessageId(
  messages: readonly Message[],
): string | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "assistant") continue;
    if (m.pending) return null;
    return m.runId ? m.id : null;
  }
  return null;
}
