import { useMemo, useState } from "react";
import {
  buildFilteredDiff,
  parseUnifiedDiff,
  type DiffHunk,
  type HunkLineSelection,
} from "@/lib/diff";

/**
 * The current review state, recomputed on every toggle.
 *
 *  - `acceptedHunks`: 0-based indices of hunks the user wants applied *whole*
 *    (no line-level edits). This is the legacy fast path — when every chosen
 *    hunk is whole, the caller forwards just these indices to the gateway.
 *  - `partial`: true when at least one hunk has a line-level subset selected,
 *    so the caller must apply `filteredDiff` instead of the index list.
 *  - `filteredDiff`: the rebuilt unified-diff text containing exactly the
 *    chosen lines across all hunks. `""` when nothing is selected.
 *  - `acceptedLineCount` / `totalLineCount`: changed-line tallies for the
 *    summary copy.
 */
export interface HunkSelection {
  acceptedHunks: number[];
  partial: boolean;
  filteredDiff: string;
  acceptedLineCount: number;
  totalLineCount: number;
}

interface Props {
  /** Raw unified-diff text from the tool call's `diff` arg. */
  diff: string;
  /**
   * Legacy callback — reports the whole-hunk selection (0-based indices,
   * sorted ascending). Kept for callers that only care about whole hunks.
   */
  onChange?: (selectedIndices: number[]) => void;
  /**
   * Richer callback — fires alongside `onChange` and carries the line-level
   * state so the caller can forward a filtered patch when the user dives in
   * below the hunk level.
   */
  onSelectionChange?: (selection: HunkSelection) => void;
}

/** Is this row one the user can individually accept/reject? */
function isToggleableRow(kind: DiffHunk["rows"][number]["kind"]): boolean {
  return kind === "add" || kind === "del";
}

/** Indices (into `hunk.rows`) of the add/del rows in a hunk. */
function changedRowIndices(hunk: DiffHunk): number[] {
  const out: number[] = [];
  hunk.rows.forEach((r, i) => {
    if (isToggleableRow(r.kind)) out.push(i);
  });
  return out;
}

/**
 * Renders a unified diff as a list of accept/reject-able hunks. Each hunk
 * starts fully accepted; toggling re-emits the selection upstream so the
 * approval prompt can forward only the chosen subset to the gateway.
 *
 * Two granularities:
 *  - whole-hunk accept/reject (the default, fast path), and
 *  - line-level: expand a hunk to toggle individual added/removed lines. A
 *    hunk with a line subset is applied via a rebuilt patch rather than its
 *    index. Matches the existing `hunk-*` zinc/amber styling.
 */
export function HunkReview({ diff, onChange, onSelectionChange }: Props) {
  const parsed = useMemo(() => parseUnifiedDiff(diff), [diff]);

  // Per-hunk accepted CHANGED-row indices. A hunk maps to the set of its
  // add/del row indices the user wants; it starts with all of them. A hunk
  // missing from the map is treated as "all changed rows accepted".
  const [accepted, setAccepted] = useState<Map<number, Set<number>>>(() => {
    const m = new Map<number, Set<number>>();
    parsed.hunks.forEach((h, i) => {
      m.set(i, new Set(changedRowIndices(h)));
    });
    return m;
  });

  // Which hunks are expanded to the line-level view.
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());

  const totalChangedRows = useMemo(
    () =>
      parsed.hunks.reduce((acc, h) => acc + changedRowIndices(h).length, 0),
    [parsed.hunks],
  );

  function emit(next: Map<number, Set<number>>) {
    let acceptedLineCount = 0;
    let partial = false;
    const wholeHunks: number[] = [];

    parsed.hunks.forEach((h, i) => {
      const changed = changedRowIndices(h);
      const sel = next.get(i) ?? new Set(changed);
      const kept = changed.filter((ri) => sel.has(ri));
      acceptedLineCount += kept.length;
      if (kept.length === changed.length && changed.length > 0) {
        wholeHunks.push(i);
      } else if (kept.length > 0) {
        partial = true;
      }
      // kept.length === 0 → hunk dropped entirely; not whole, not partial.
    });

    // Build the filtered patch only when needed (a partial selection or a
    // mix where a whole hunk was dropped). When every selected hunk is whole
    // and none were dropped, the legacy index path covers it.
    const filteredDiff = buildFilteredDiff(parsed.hunks, next);

    onChange?.(wholeHunks);
    onSelectionChange?.({
      acceptedHunks: wholeHunks,
      partial,
      filteredDiff,
      acceptedLineCount,
      totalLineCount: totalChangedRows,
    });
  }

  function update(mutator: (m: Map<number, Set<number>>) => void) {
    setAccepted((prev) => {
      const next = new Map<number, Set<number>>();
      prev.forEach((v, k) => next.set(k, new Set(v)));
      mutator(next);
      emit(next);
      return next;
    });
  }

  function toggleHunk(idx: number) {
    update((m) => {
      const changed = changedRowIndices(parsed.hunks[idx]);
      const sel = m.get(idx) ?? new Set(changed);
      // If anything is currently accepted, reject the whole hunk; else accept.
      if (sel.size > 0) m.set(idx, new Set());
      else m.set(idx, new Set(changed));
    });
  }

  function toggleLine(hunkIdx: number, rowIdx: number) {
    update((m) => {
      const changed = changedRowIndices(parsed.hunks[hunkIdx]);
      const sel = m.get(hunkIdx) ?? new Set(changed);
      if (sel.has(rowIdx)) sel.delete(rowIdx);
      else sel.add(rowIdx);
      m.set(hunkIdx, sel);
    });
  }

  function setHunkAll(hunkIdx: number, state: boolean) {
    update((m) => {
      const changed = changedRowIndices(parsed.hunks[hunkIdx]);
      m.set(hunkIdx, state ? new Set(changed) : new Set());
    });
  }

  function setAll(state: boolean) {
    update((m) => {
      parsed.hunks.forEach((h, i) => {
        m.set(i, state ? new Set(changedRowIndices(h)) : new Set());
      });
    });
  }

  function toggleExpanded(idx: number) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  }

  // Derived per-render tallies for the head summary.
  const acceptedRowCount = useMemo(() => {
    let n = 0;
    parsed.hunks.forEach((h, i) => {
      const changed = changedRowIndices(h);
      const sel = accepted.get(i) ?? new Set(changed);
      n += changed.filter((ri) => sel.has(ri)).length;
    });
    return n;
  }, [accepted, parsed.hunks]);

  if (parsed.hunks.length === 0) {
    return (
      <div className="hunk-review hunk-review-empty">
        No reviewable changes in this diff — nothing to accept or reject.
      </div>
    );
  }

  const noneSelected = acceptedRowCount === 0;
  const allSelected = acceptedRowCount === totalChangedRows;

  return (
    <div className="hunk-review">
      <div className="hunk-review-head">
        <span className="hunk-review-summary">
          {acceptedRowCount} of {totalChangedRows} changed{" "}
          {totalChangedRows === 1 ? "line" : "lines"} selected
        </span>
        <div className="hunk-review-bulk">
          <button
            type="button"
            className="link-btn"
            onClick={() => setAll(true)}
            disabled={allSelected}
          >
            Select all
          </button>
          <button
            type="button"
            className="link-btn"
            onClick={() => setAll(false)}
            disabled={noneSelected}
          >
            Clear
          </button>
        </div>
      </div>
      <ol className="hunk-list">
        {parsed.hunks.map((hunk, i) => {
          const changed = changedRowIndices(hunk);
          const sel = accepted.get(i) ?? new Set(changed);
          const keptCount = changed.filter((ri) => sel.has(ri)).length;
          return (
            <HunkBlock
              key={i}
              hunk={hunk}
              index={i}
              changedRows={changed}
              selected={sel}
              keptCount={keptCount}
              expanded={expanded.has(i)}
              onToggleHunk={() => toggleHunk(i)}
              onToggleExpanded={() => toggleExpanded(i)}
              onToggleLine={(ri) => toggleLine(i, ri)}
              onSetAll={(state) => setHunkAll(i, state)}
            />
          );
        })}
      </ol>
    </div>
  );
}

interface BlockProps {
  hunk: DiffHunk;
  index: number;
  changedRows: number[];
  selected: HunkLineSelection;
  keptCount: number;
  expanded: boolean;
  onToggleHunk: () => void;
  onToggleExpanded: () => void;
  onToggleLine: (rowIdx: number) => void;
  onSetAll: (state: boolean) => void;
}

function HunkBlock({
  hunk,
  index,
  changedRows,
  selected,
  keptCount,
  expanded,
  onToggleHunk,
  onToggleExpanded,
  onToggleLine,
  onSetAll,
}: BlockProps) {
  const total = changedRows.length;
  const state: "accepted" | "rejected" | "partial" =
    keptCount === 0 ? "rejected" : keptCount === total ? "accepted" : "partial";
  const stateLabel =
    state === "accepted"
      ? "Accepted"
      : state === "rejected"
        ? "Rejected"
        : `${keptCount}/${total} lines`;

  return (
    <li className={`hunk-block ${state === "rejected" ? "hunk-rejected" : ""}`}>
      <div className="hunk-head">
        <span className="hunk-tag">hunk {index + 1}</span>
        <code className="hunk-coords">
          @@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@
        </code>
        <button
          type="button"
          className="hunk-lines-toggle link-btn"
          onClick={onToggleExpanded}
          aria-expanded={expanded}
          title={
            expanded
              ? "Hide individual lines"
              : "Pick individual lines to accept"
          }
        >
          {expanded ? "Hide lines" : "Pick lines"}
        </button>
        <button
          type="button"
          className={`hunk-toggle is-${state}`}
          onClick={onToggleHunk}
          title={
            state === "rejected"
              ? "Accept this hunk"
              : "Reject this hunk"
          }
        >
          {stateLabel}
        </button>
      </div>
      <pre className="hunk-body">
        {hunk.rows.map((row, ri) => {
          const toggleable = isToggleableRow(row.kind);
          const lineKept = !toggleable || selected.has(ri);
          const rowClass = `hunk-row hunk-row-${row.kind}${
            toggleable && !lineKept ? " hunk-row-deselected" : ""
          }`;
          if (!expanded || !toggleable) {
            return (
              <div key={ri} className={rowClass}>
                {expanded && <span className="hunk-line-check-spacer" />}
                <span className="hunk-marker">
                  {row.kind === "add" ? "+" : row.kind === "del" ? "-" : " "}
                </span>
                <span className="hunk-text">{row.text}</span>
              </div>
            );
          }
          return (
            <label key={ri} className={`${rowClass} hunk-row-pickable`}>
              <input
                type="checkbox"
                className="hunk-line-check"
                checked={lineKept}
                onChange={() => onToggleLine(ri)}
              />
              <span className="hunk-marker">
                {row.kind === "add" ? "+" : "-"}
              </span>
              <span className="hunk-text">{row.text}</span>
            </label>
          );
        })}
      </pre>
      {expanded && (
        <div className="hunk-line-bulk">
          <button
            type="button"
            className="link-btn"
            onClick={() => onSetAll(true)}
            disabled={keptCount === total}
          >
            All lines
          </button>
          <button
            type="button"
            className="link-btn"
            onClick={() => onSetAll(false)}
            disabled={keptCount === 0}
          >
            No lines
          </button>
        </div>
      )}
    </li>
  );
}
