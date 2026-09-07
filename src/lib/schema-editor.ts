/**
 * Lightweight JSON parser and pseudo-schema helper for the SchemaEditor
 * modal. We deliberately avoid a runtime JSON Schema library — the modal's
 * schema panel is a static hint string, and validation just needs to point
 * the user at the first parse error.
 */

export interface ParseError {
  /** 1-based line number derived from the JSON.parse position. */
  line: number;
  /** 1-based column within `line`. */
  column: number;
  /** Original parser message, trimmed. */
  message: string;
}

export interface ParseOutcome {
  ok: boolean;
  /** `null` when the body is empty whitespace; otherwise the parsed value. */
  value: unknown;
  error: ParseError | null;
}

/**
 * Parse a JSON body, returning a structured error with line:col offsets the
 * editor can render in the gutter. An empty / whitespace-only body counts as
 * "ok" with a null value — we don't want to spam the gutter while the user
 * is mid-edit.
 */
export function parseJSON(body: string): ParseOutcome {
  const trimmed = body.trim();
  if (trimmed.length === 0) {
    return { ok: true, value: null, error: null };
  }
  try {
    const value = JSON.parse(body);
    return { ok: true, value, error: null };
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    // SpiderMonkey reports line/column directly in the parser message; use
    // those verbatim rather than deriving from a byte offset.
    const sm = message.match(/at line\s+(\d+)\s+column\s+(\d+)/i);
    const pos = sm ? null : extractPosition(message);
    const { line, column } = sm
      ? { line: Number(sm[1]), column: Number(sm[2]) }
      : pos
        ? offsetToLineColumn(body, pos)
        : { line: 1, column: 1 };
    return {
      ok: false,
      value: undefined,
      error: { line, column, message: message.trim() },
    };
  }
}

/**
 * Try to pull a character offset out of a JSON.parse error message. V8 and
 * SpiderMonkey emit slightly different shapes, so we accept either.
 */
function extractPosition(message: string): number | null {
  // V8: "Unexpected token } in JSON at position 42"
  const v8 = message.match(/position\s+(\d+)/i);
  if (v8) return Number(v8[1]);
  // SpiderMonkey: "JSON.parse: ... at line 3 column 5 of the JSON data"
  const sm = message.match(/at line\s+(\d+)\s+column\s+(\d+)/i);
  if (sm) {
    // We don't need the offset because we can return line/col directly.
    // Encode via a sentinel: convert back below.
    return -1;
  }
  return null;
}

/**
 * Convert a byte offset within `body` into a 1-based (line, column) pair.
 * Treats `\r\n` and `\n` as one logical line break.
 */
export function offsetToLineColumn(
  body: string,
  offset: number,
): { line: number; column: number } {
  // Sentinel from extractPosition: SpiderMonkey supplies line/col directly,
  // so we re-parse it here.
  if (offset === -1) {
    const sm = body.match(/at line\s+(\d+)\s+column\s+(\d+)/i);
    if (sm) return { line: Number(sm[1]), column: Number(sm[2]) };
  }
  if (offset < 0) return { line: 1, column: 1 };
  let line = 1;
  let column = 1;
  const max = Math.min(offset, body.length);
  for (let i = 0; i < max; i++) {
    const ch = body.charCodeAt(i);
    if (ch === 0x0a) {
      line += 1;
      column = 1;
    } else if (ch === 0x0d) {
      // Swallow \r\n as one line break.
      if (i + 1 < body.length && body.charCodeAt(i + 1) === 0x0a) i += 1;
      line += 1;
      column = 1;
    } else {
      column += 1;
    }
  }
  return { line, column };
}

/**
 * Compute the visible line count for the gutter. Always at least 1 so a
 * blank editor still shows a `1` line number.
 */
export function lineCount(body: string): number {
  if (body.length === 0) return 1;
  let n = 1;
  for (let i = 0; i < body.length; i++) {
    const ch = body.charCodeAt(i);
    if (ch === 0x0a) n += 1;
  }
  return n;
}

/**
 * Pretty-print a parsed JSON value with stable 2-space indent. Returns the
 * input unchanged if parsing fails so the user's malformed body isn't
 * silently destroyed.
 */
export function prettify(body: string): string {
  const outcome = parseJSON(body);
  if (!outcome.ok || outcome.value === null) return body;
  try {
    return `${JSON.stringify(outcome.value, null, 2)}\n`;
  } catch {
    return body;
  }
}
