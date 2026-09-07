import { useEffect, useMemo, useState } from "react";
import { sideBySideFromText, type SideBySideRow } from "@/lib/diff";
import {
  formatBytes,
  timeAgo,
  type CheckpointDiff,
  type CheckpointDiffEntry,
  type CheckpointInfo,
} from "@/lib/checkpoints";

/**
 * Pre-restore confirmation surface: shows what restoring a checkpoint would do
 * to the live worktree (added / modified / removed counts + per-file diffs)
 * and lets the user confirm or cancel. The diff is computed read-only by the
 * `diff_checkpoint` command — nothing is mutated until the user confirms and
 * the caller runs the actual restore.
 *
 * Diff rendering reuses `sideBySideFromText` from `@/lib/diff` (the same engine
 * the Composer review panel uses). Large diffs are handled gracefully: each
 * file is collapsed by default with a +/- summary, and very long files clip to
 * a row cap with a "show more" expander.
 */

interface Props {
  checkpoint: CheckpointInfo;
  diff: CheckpointDiff;
  /** True while the actual restore is in flight after confirm. */
  restoring: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Rows shown before a single file's diff clips behind "show more". */
const ROW_CLIP = 200;

function StatusBadge({ status }: { status: CheckpointDiffEntry["status"] }) {
  return (
    <span className={`ckdiff-badge ckdiff-badge-${status}`}>
      {status === "added"
        ? "restored"
        : status === "modified"
          ? "overwritten"
          : "kept"}
    </span>
  );
}

function SideBySide({ rows, limit }: { rows: SideBySideRow[]; limit: number }) {
  const clipped = rows.slice(0, limit);
  return (
    <div className="ckdiff-split">
      {clipped.map((r, i) => {
        const leftCls =
          r.kind === "context" ? "ctx" : r.kind === "add" ? "empty" : "del";
        const rightCls =
          r.kind === "context" ? "ctx" : r.kind === "del" ? "empty" : "add";
        return (
          <div className="ckdiff-row" key={`s-${i}`}>
            <span className="ckdiff-gutter">{r.oldLine ?? ""}</span>
            <span className={`ckdiff-cell ${leftCls}`}>
              {r.oldText == null ? "" : r.oldText || " "}
            </span>
            <span className="ckdiff-gutter">{r.newLine ?? ""}</span>
            <span className={`ckdiff-cell ${rightCls}`}>
              {r.newText == null ? "" : r.newText || " "}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function FileRow({ entry }: { entry: CheckpointDiffEntry }) {
  const [open, setOpen] = useState(false);
  const [showAll, setShowAll] = useState(false);

  const rows = useMemo<SideBySideRow[]>(() => {
    if (entry.binary) return [];
    // `old` = current worktree, `new` = checkpoint (what restore writes).
    return sideBySideFromText(entry.old_content ?? "", entry.new_content ?? "");
  }, [entry.binary, entry.old_content, entry.new_content]);

  const total = rows.length;
  const visibleLimit = showAll ? total : Math.min(total, ROW_CLIP);
  const remaining = Math.max(0, total - visibleLimit);

  const sizeNote =
    entry.status === "added"
      ? formatBytes(entry.new_size)
      : entry.status === "removed"
        ? formatBytes(entry.old_size)
        : `${formatBytes(entry.old_size)} → ${formatBytes(entry.new_size)}`;

  return (
    <li className="ckdiff-file">
      <button
        type="button"
        className="ckdiff-file-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="ckdiff-caret">{open ? "▾" : "▸"}</span>
        <StatusBadge status={entry.status} />
        <code className="ckdiff-path">{entry.path}</code>
        <span className="ckdiff-size">{sizeNote}</span>
      </button>
      {open && (
        <div className="ckdiff-body">
          {entry.binary ? (
            <p className="ckdiff-binary">
              Binary or too large to diff — restore would{" "}
              {entry.status === "added"
                ? "create this file"
                : entry.status === "removed"
                  ? "leave this file untouched"
                  : "overwrite this file"}
              .
            </p>
          ) : total === 0 ? (
            <p className="ckdiff-binary">No textual differences.</p>
          ) : (
            <>
              <SideBySide rows={rows} limit={visibleLimit} />
              {remaining > 0 && !showAll && (
                <button
                  type="button"
                  className="link-btn ckdiff-more"
                  onClick={() => setShowAll(true)}
                >
                  Show {remaining} more line{remaining === 1 ? "" : "s"}
                </button>
              )}
            </>
          )}
        </div>
      )}
    </li>
  );
}

export function CheckpointDiffModal({
  checkpoint,
  diff,
  restoring,
  onConfirm,
  onCancel,
}: Props) {
  // ESC cancels — standard transient-surface behaviour. Disabled mid-restore so
  // a stray keypress can't desync the UI from an in-flight write.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !restoring) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, restoring]);

  const noChanges = diff.entries.length === 0;
  const label = checkpoint.label ?? "auto";

  return (
    <div
      className="ckdiff-backdrop"
      onMouseDown={() => {
        if (!restoring) onCancel();
      }}
    >
      <div
        className="ckdiff-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="ckdiff-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="ckdiff-header">
          <div>
            <h2 id="ckdiff-title">Restore checkpoint</h2>
            <p className="ckdiff-sub">
              <strong>{label}</strong> · {timeAgo(checkpoint.ts)} ·{" "}
              <code>{checkpoint.id.slice(0, 12)}</code>
            </p>
          </div>
          <button
            className="ckdiff-close"
            onClick={onCancel}
            disabled={restoring}
            aria-label="Cancel"
          >
            ×
          </button>
        </header>

        <div className="ckdiff-summary" role="status">
          {noChanges ? (
            <span>
              This checkpoint matches your current files — restoring would change
              nothing.
            </span>
          ) : (
            <>
              <span className="ckdiff-stat ckdiff-stat-modified">
                {diff.modified} overwritten
              </span>
              <span className="ckdiff-stat ckdiff-stat-added">
                {diff.added} restored
              </span>
              <span className="ckdiff-stat ckdiff-stat-removed">
                {diff.removed} kept
              </span>
              <span className="ckdiff-hint">
                Restore overwrites current files with the checkpoint. "Kept" files
                exist now but not in the checkpoint — they survive untouched.
              </span>
            </>
          )}
        </div>

        <div className="ckdiff-list">
          {noChanges ? (
            <p className="ckdiff-empty">
              Nothing to compare. You can still restore to be safe, but your tree
              already matches this checkpoint.
            </p>
          ) : (
            <ul>
              {diff.entries.map((e) => (
                <FileRow key={`${e.status}:${e.path}`} entry={e} />
              ))}
            </ul>
          )}
        </div>

        <footer className="ckdiff-footer">
          <button
            className="ckdiff-secondary"
            onClick={onCancel}
            disabled={restoring}
          >
            Cancel
          </button>
          <button
            className="ckdiff-confirm"
            onClick={onConfirm}
            disabled={restoring}
          >
            {restoring
              ? "Restoring…"
              : noChanges
                ? "Restore anyway"
                : "Restore — overwrite current files"}
          </button>
        </footer>
      </div>
    </div>
  );
}
