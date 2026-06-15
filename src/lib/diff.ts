// Tiny zero-dependency diff helpers for the Composer review panel.
//
// We deliberately keep this small and "good enough" instead of pulling in
// the `diff` npm package. Two surfaces are supported:
//
//  1. parseUnifiedDiff()   — parses a unified-diff text into hunks with
//                            rows tagged as add / del / context / header.
//                            Used when the gateway streams a real patch.
//
//  2. sideBySideFromText() — given old/new file contents (whole files), it
//                            walks them line-by-line and emits a single
//                            ordered row list where each row's `kind`
//                            (add | del | context) is determined by a
//                            naive longest-common-subsequence walk. The
//                            renderer pairs adjacent del/add rows into
//                            two-column "modified" rows; everything else
//                            renders as a single row spanning both columns.

export type DiffRowKind = "add" | "del" | "context" | "header";

export interface DiffRow {
  kind: DiffRowKind;
  /** The visible text content of the row (no leading +/-/space marker). */
  text: string;
  /** 1-based line number in the OLD file, if known. */
  oldLine: number | null;
  /** 1-based line number in the NEW file, if known. */
  newLine: number | null;
}

export interface DiffHunk {
  header: string;
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  rows: DiffRow[];
}

export interface ParsedDiff {
  hunks: DiffHunk[];
  /** Total number of visible (non-header) rows across all hunks. */
  totalRows: number;
}

const HUNK_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@(.*)$/;

/**
 * Parse a unified-diff string (the kind `git diff` produces) into hunks.
 * File-header lines like `--- a/foo`, `+++ b/foo`, `diff --git ...` and
 * `index ...` are skipped — we only care about the @@ hunks and their
 * payload rows.
 */
export function parseUnifiedDiff(diff: string): ParsedDiff {
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let oldLine = 0;
  let newLine = 0;

  const lines = diff.split(/\r?\n/);
  for (const raw of lines) {
    if (raw.startsWith("@@ ") || raw.startsWith("@@\t")) {
      const m = HUNK_RE.exec(raw);
      if (!m) continue;
      const oldStart = Number(m[1]);
      const oldCount = m[2] != null ? Number(m[2]) : 1;
      const newStart = Number(m[3]);
      const newCount = m[4] != null ? Number(m[4]) : 1;
      const headerTail = (m[5] ?? "").trim();
      current = {
        header: headerTail || raw.trim(),
        oldStart,
        oldCount,
        newStart,
        newCount,
        rows: [],
      };
      hunks.push(current);
      oldLine = oldStart;
      newLine = newStart;
      continue;
    }

    if (!current) {
      // pre-amble lines (file headers) — ignore
      continue;
    }

    if (raw.startsWith("+++") || raw.startsWith("---")) {
      continue; // stray file headers inside the diff body
    }

    if (raw.startsWith("\\")) {
      // "\ No newline at end of file" — surface as a context row so the
      // user sees it but don't advance line counters.
      current.rows.push({
        kind: "context",
        text: raw,
        oldLine: null,
        newLine: null,
      });
      continue;
    }

    const marker = raw.charAt(0);
    const text = raw.slice(1);
    if (marker === "+") {
      current.rows.push({ kind: "add", text, oldLine: null, newLine });
      newLine += 1;
    } else if (marker === "-") {
      current.rows.push({ kind: "del", text, oldLine, newLine: null });
      oldLine += 1;
    } else if (marker === " ") {
      current.rows.push({ kind: "context", text, oldLine, newLine });
      oldLine += 1;
      newLine += 1;
    } else if (raw === "") {
      // Empty line at end of hunk — treat as context.
      current.rows.push({ kind: "context", text: "", oldLine, newLine });
      oldLine += 1;
      newLine += 1;
    } else {
      // Unknown line marker — treat as header noise.
      current.rows.push({
        kind: "header",
        text: raw,
        oldLine: null,
        newLine: null,
      });
    }
  }

  const totalRows = hunks.reduce((acc, h) => acc + h.rows.length, 0);
  return { hunks, totalRows };
}

/**
 * A line-level selection for one hunk: the set of row indices (into
 * `DiffHunk.rows`) the user wants to KEEP. Only `add`/`del` rows are
 * meaningful here — `context`/`header` rows are always carried through.
 */
export type HunkLineSelection = Set<number>;

/**
 * Re-serialise a parsed diff into unified-diff text, applying a per-hunk
 * line selection. This deliberately reuses {@link parseUnifiedDiff}'s output
 * rather than introducing a second diff engine — the caller hands us the
 * already-parsed hunks plus, for each hunk index, the set of row indices to
 * keep.
 *
 * Semantics for an *unselected* row:
 *  - an unselected `add` is simply dropped (the line is never inserted)
 *  - an unselected `del` is downgraded to context (the line stays in the
 *    file unchanged) so the surrounding patch still applies cleanly
 *
 * `selections.get(i)` of `undefined` means "keep the whole hunk i verbatim".
 * A hunk that ends up with zero real changes (every add dropped, every del
 * kept) is omitted entirely. Hunk headers (`@@ -a,b +c,d @@`) are recomputed
 * from the surviving rows so the patch is internally consistent.
 *
 * Returns `""` when nothing remains — the caller should treat that as "deny".
 */
export function buildFilteredDiff(
  hunks: DiffHunk[],
  selections: Map<number, HunkLineSelection>,
): string {
  const out: string[] = [];

  hunks.forEach((hunk, hi) => {
    const sel = selections.get(hi);
    // Decide the effective kind of each row under the selection.
    type Emit = { kind: "add" | "del" | "context"; text: string };
    const emitted: Emit[] = [];
    let realChanges = 0;

    hunk.rows.forEach((row, ri) => {
      if (row.kind === "header") return; // never emitted into a hunk body
      // "\ No newline at end of file" markers were stored as context with a
      // leading backslash — pass them through untouched.
      if (row.kind === "context") {
        emitted.push({ kind: "context", text: row.text });
        return;
      }
      const kept = sel == null || sel.has(ri);
      if (row.kind === "add") {
        if (kept) {
          emitted.push({ kind: "add", text: row.text });
          realChanges += 1;
        }
        // dropped add → contributes nothing
      } else {
        // del
        if (kept) {
          emitted.push({ kind: "del", text: row.text });
          realChanges += 1;
        } else {
          // keep the line in the file unchanged
          emitted.push({ kind: "context", text: row.text });
        }
      }
    });

    if (realChanges === 0) return; // nothing left to apply in this hunk

    // Recompute line counts. Old side = context + del; new side = context + add.
    let oldCount = 0;
    let newCount = 0;
    for (const e of emitted) {
      if (e.kind === "context") {
        oldCount += 1;
        newCount += 1;
      } else if (e.kind === "del") {
        oldCount += 1;
      } else {
        newCount += 1;
      }
    }

    // oldStart/newStart are preserved from the source hunk — the leading
    // context anchors the patch at the same place.
    out.push(
      `@@ -${hunk.oldStart},${oldCount} +${hunk.newStart},${newCount} @@`,
    );
    for (const e of emitted) {
      const marker = e.kind === "add" ? "+" : e.kind === "del" ? "-" : " ";
      out.push(`${marker}${e.text}`);
    }
  });

  if (out.length === 0) return "";
  return out.join("\n") + "\n";
}

/**
 * Build a LCS table for two arrays of strings. Returns the table so
 * sideBySideFromText() can walk it backwards to emit aligned rows.
 *
 * Note: this is O(n*m) in time and memory. We cap input to ~5000 lines
 * each below before calling this; for anything larger the caller falls
 * back to a coarser "old block then new block" view.
 */
function buildLCS(a: string[], b: string[]): Uint32Array {
  const n = a.length;
  const m = b.length;
  const w = m + 1;
  const dp = new Uint32Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      if (a[i] === b[j]) {
        dp[i * w + j] = dp[(i + 1) * w + (j + 1)] + 1;
      } else {
        const down = dp[(i + 1) * w + j];
        const right = dp[i * w + (j + 1)];
        dp[i * w + j] = down >= right ? down : right;
      }
    }
  }
  return dp;
}

export interface SideBySideRow {
  kind: "context" | "modified" | "add" | "del";
  /** Left column (old). Empty for pure adds. */
  oldText: string | null;
  /** Right column (new). Empty for pure dels. */
  newText: string | null;
  oldLine: number | null;
  newLine: number | null;
}

/** Hard ceiling on either side's line count — beyond this we degrade gracefully. */
const LCS_LIMIT = 5000;
/**
 * Hard ceiling on the LCS table size (n*m). `buildLCS` allocates one entry
 * per (oldLine, newLine) pair, so two files each just under `LCS_LIMIT`
 * would still allocate ~25M entries. Cap the product as well.
 */
const LCS_CELL_LIMIT = 4_000_000;

/**
 * Walk `oldContent` and `newContent` line-by-line and emit a unified list
 * of rows. Adjacent (del, add) pairs are collapsed into a single
 * `modified` row so the side-by-side renderer can show them on one
 * line. Pure insertions/deletions get their own row.
 */
export function sideBySideFromText(
  oldContent: string,
  newContent: string,
): SideBySideRow[] {
  const a = oldContent.split(/\r?\n/);
  const b = newContent.split(/\r?\n/);

  // For very large files, fall back to a dumb "del everything / add
  // everything" view rather than allocating a multi-megabyte LCS table.
  if (
    a.length > LCS_LIMIT ||
    b.length > LCS_LIMIT ||
    a.length * b.length > LCS_CELL_LIMIT
  ) {
    const rows: SideBySideRow[] = [];
    const cap = Math.max(a.length, b.length);
    for (let i = 0; i < cap; i++) {
      const o = i < a.length ? a[i] : null;
      const n = i < b.length ? b[i] : null;
      if (o === n && o != null) {
        rows.push({
          kind: "context",
          oldText: o,
          newText: n,
          oldLine: i + 1,
          newLine: i + 1,
        });
      } else {
        rows.push({
          kind: "modified",
          oldText: o,
          newText: n,
          oldLine: o != null ? i + 1 : null,
          newLine: n != null ? i + 1 : null,
        });
      }
    }
    return rows;
  }

  const dp = buildLCS(a, b);
  const w = b.length + 1;
  // Walk forward emitting raw add/del/context, then collapse adjacent
  // del+add into a single "modified" row in a second pass.
  type Raw = { kind: "add" | "del" | "context"; oldLine: number | null; newLine: number | null; text: string };
  const raw: Raw[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      raw.push({ kind: "context", oldLine: i + 1, newLine: j + 1, text: a[i] });
      i += 1;
      j += 1;
    } else if (dp[(i + 1) * w + j] >= dp[i * w + (j + 1)]) {
      raw.push({ kind: "del", oldLine: i + 1, newLine: null, text: a[i] });
      i += 1;
    } else {
      raw.push({ kind: "add", oldLine: null, newLine: j + 1, text: b[j] });
      j += 1;
    }
  }
  while (i < a.length) {
    raw.push({ kind: "del", oldLine: i + 1, newLine: null, text: a[i] });
    i += 1;
  }
  while (j < b.length) {
    raw.push({ kind: "add", oldLine: null, newLine: j + 1, text: b[j] });
    j += 1;
  }

  // Collapse: a run of K dels followed by K adds becomes K modified rows;
  // unequal-length runs leave any remainder as standalone add/del rows.
  const out: SideBySideRow[] = [];
  let k = 0;
  while (k < raw.length) {
    const row = raw[k];
    if (row.kind === "context") {
      out.push({
        kind: "context",
        oldText: row.text,
        newText: row.text,
        oldLine: row.oldLine,
        newLine: row.newLine,
      });
      k += 1;
      continue;
    }
    // Gather a run of dels then a run of adds (or vice versa).
    let delsStart = k;
    let delsEnd = k;
    while (delsEnd < raw.length && raw[delsEnd].kind === "del") delsEnd += 1;
    let addsEnd = delsEnd;
    while (addsEnd < raw.length && raw[addsEnd].kind === "add") addsEnd += 1;

    const dels = raw.slice(delsStart, delsEnd);
    const adds = raw.slice(delsEnd, addsEnd);

    // Pair them up.
    const pair = Math.min(dels.length, adds.length);
    for (let p = 0; p < pair; p++) {
      const d = dels[p];
      const ad = adds[p];
      out.push({
        kind: "modified",
        oldText: d.text,
        newText: ad.text,
        oldLine: d.oldLine,
        newLine: ad.newLine,
      });
    }
    // Leftover dels.
    for (let p = pair; p < dels.length; p++) {
      const d = dels[p];
      out.push({
        kind: "del",
        oldText: d.text,
        newText: null,
        oldLine: d.oldLine,
        newLine: null,
      });
    }
    // Leftover adds.
    for (let p = pair; p < adds.length; p++) {
      const ad = adds[p];
      out.push({
        kind: "add",
        oldText: null,
        newText: ad.text,
        oldLine: null,
        newLine: ad.newLine,
      });
    }
    k = addsEnd;
  }

  return out;
}
