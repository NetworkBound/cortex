/**
 * @-picker value-insertion helpers.
 *
 * The FilePicker emits a string `value` that is either a plain filename
 * (legacy `files` kind) or a `<kind>:<inner>` envelope. ChatPane delegates
 * the actual composer text expansion to the helpers here so the component
 * stays under the 500-LOC cap.
 *
 * Two envelopes get full expansion (block insertion):
 *   - `thread:<session_id>`     → pulls up to 50 recent messages and serializes
 *                                  them between `--- thread … ---` fences.
 *   - `diagnostic:<id>`         → looks up the matching issue/crash row and
 *                                  emits a `## Diagnostic` block (stack capped).
 *
 * Anything else is inserted verbatim as `@<value> ` at the original `@` anchor.
 */

import { loadSessionMessages } from "@/lib/sessions";
import { recentIssues, recentCrashes } from "@/lib/observability";

// Cap pulled thread messages to keep model context lean.
export const THREAD_MSG_CAP = 50;
// Cap stack trace lines on diagnostic insertions.
export const DIAG_STACK_LINE_CAP = 200;

/** Build the `--- thread … ---` block (or a sentinel if loading fails). */
export async function buildThreadBlock(sessionId: string): Promise<string> {
  try {
    const msgs = await loadSessionMessages(sessionId);
    const capped = msgs.slice(-THREAD_MSG_CAP);
    const lines = capped.map((m) => `${m.role}: ${m.content}`).join("\n");
    return `--- thread ${sessionId} ---\n${lines}\n--- end thread ---\n`;
  } catch (err) {
    console.warn("loadSessionMessages failed", err);
    return `--- thread ${sessionId} ---\n[empty]\n--- end thread ---\n`;
  }
}

/** Build the `## Diagnostic` block by matching `id` against issues + crashes. */
export async function buildDiagnosticBlock(id: string): Promise<string> {
  try {
    const [issues, crashes] = await Promise.all([
      recentIssues(50),
      recentCrashes(50),
    ]);
    const issue = issues.find((i) => i.fingerprint === id);
    if (issue) {
      return (
        `## Diagnostic\n` +
        `kind: issue\n` +
        `message: ${issue.message}\n` +
        `error_class: ${issue.error_class ?? ""}\n` +
        `count: ${issue.count}\n`
      );
    }
    const crash = crashes.find((c) => String(c.id) === id);
    if (crash) {
      const stack = (crash.stack ?? "")
        .split("\n")
        .slice(0, DIAG_STACK_LINE_CAP)
        .join("\n");
      return (
        `## Diagnostic\n` +
        `kind: ${crash.kind}\n` +
        `message: ${crash.message}\n` +
        `stack: ${stack}\n`
      );
    }
  } catch (err) {
    console.warn("diagnostic lookup failed", err);
  }
  return `## Diagnostic\nid: ${id}\n[not found]\n`;
}

export interface InsertResult {
  /** Replacement text for the entire composer input. */
  text: string;
  /** Optional caret position; undefined means "leave caret to the browser". */
  caret?: number;
}

/**
 * Splice an envelope value into `input` at the `@` anchor index. Async because
 * thread / diagnostic expansion hits Tauri commands. The `@…` token (anchor to
 * next whitespace) is replaced wholesale.
 */
export async function applyPickedValue(
  input: string,
  anchor: number,
  value: string,
): Promise<InsertResult> {
  const before = input.slice(0, anchor);
  const after = input.slice(anchor);
  const tokenEnd = after.search(/\s/);
  const tail = tokenEnd === -1 ? "" : after.slice(tokenEnd);

  const threadM = value.match(/^thread:(.+)$/);
  if (threadM) {
    const block = await buildThreadBlock(threadM[1]);
    const trimmedTail = tail.trimStart();
    return {
      text: `${before}${block}${trimmedTail ? " " + trimmedTail : ""}`,
    };
  }

  const diagM = value.match(/^diagnostic:(.+)$/);
  if (diagM) {
    const block = await buildDiagnosticBlock(diagM[1]);
    const trimmedTail = tail.trimStart();
    return {
      text: `${before}${block}${trimmedTail ? " " + trimmedTail : ""}`,
    };
  }

  // Default: plain filename or other `<kind>:<value>` envelope → inline `@value`.
  const insert = `@${value} `;
  return {
    text: `${before}${insert}${tail.replace(/^\s+/, "")}`,
    caret: before.length + insert.length,
  };
}
