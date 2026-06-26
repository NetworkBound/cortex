import { useCallback, useEffect, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { SkeletonText } from "./Skeleton";
import {
  createBackup,
  deleteBackup,
  formatBytes,
  listBackups,
  restoreBackup,
  timeAgo,
  type BackupMeta,
  type RestoreReport,
} from "@/lib/backup";
import { pushToast } from "@/lib/toast";

/**
 * Full backup + restore modal. Mirrors the IDEExportModal pattern: a
 * self-mounting portal so the slash command can summon it without App.tsx
 * wiring.
 *
 * Restore is intentionally two-phase — a dry-run preview lists how many
 * files would be touched before the user gets a destructive Confirm button.
 */

interface BackupPanelProps {
  onClose: () => void;
  initialFocusedId?: string | null;
}

interface PendingRestore {
  meta: BackupMeta;
  preview: RestoreReport;
}

export function BackupPanel({ onClose, initialFocusedId }: BackupPanelProps) {
  const [backups, setBackups] = useState<BackupMeta[]>([]);
  const [label, setLabel] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingRestore | null>(null);
  const [loading, setLoading] = useState(true);

  // ESC closes (unless a restore confirm is in flight — close that first).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (pending) {
        setPending(null);
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, pending]);

  const refresh = useCallback(async () => {
    try {
      setError(null);
      const items = await listBackups();
      setBackups(items);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // `/restore` opens the panel "focused" on the most recent backup. We surface
  // that by scrolling its row into view + briefly highlighting it.
  useEffect(() => {
    if (!initialFocusedId) return;
    const el = document.querySelector(
      `[data-backup-id="${cssEscape(initialFocusedId)}"]`,
    ) as HTMLElement | null;
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    el.classList.add("backup-row-focus");
    const t = window.setTimeout(() => el.classList.remove("backup-row-focus"), 1600);
    return () => window.clearTimeout(t);
  }, [initialFocusedId, backups.length]);

  const onCreate = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const meta = await createBackup(label.trim() || "manual");
      pushToast({
        title: "Backup created",
        body: `${meta.file_count} files, ${formatBytes(meta.size_bytes)}`,
        kind: "success",
      });
      setLabel("");
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [label, refresh]);

  const onRestoreClick = useCallback(async (meta: BackupMeta) => {
    setBusy(true);
    setError(null);
    try {
      const preview = await restoreBackup(meta.id, true);
      setPending({ meta, preview });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const onConfirmRestore = useCallback(async () => {
    if (!pending) return;
    setBusy(true);
    setError(null);
    try {
      const report = await restoreBackup(pending.meta.id, false);
      pushToast({
        title: "Restore complete",
        body: `Restored ${report.files_restored}, skipped ${report.files_skipped}${
          report.errors.length ? `, ${report.errors.length} error(s)` : ""
        }`,
        kind: report.errors.length === 0 ? "success" : "warning",
      });
      setPending(null);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [pending]);

  const onDelete = useCallback(
    async (meta: BackupMeta) => {
      if (!(await confirmDialog({
        title: "Delete backup?",
        message: `"${meta.label}" (${meta.id}) will be deleted.`,
        confirmLabel: "Delete",
        danger: true,
      }))) return;
      setBusy(true);
      setError(null);
      try {
        await deleteBackup(meta.id);
        pushToast({ title: "Backup deleted", body: meta.label, kind: "info" });
        await refresh();
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  return (
    <div className="backup-backdrop" onMouseDown={onClose}>
      <div
        className="backup-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="backup-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="backup-header">
          <h2 id="backup-title">Cortex Backups</h2>
          <button className="backup-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>
        <p className="backup-summary">
          Full export of <code>~/.cortex</code> + Claude project memory files.
          Saved to <code>~/.cortex/backups/</code>.
        </p>

        <div className="backup-create-row">
          <input
            className="backup-label-input"
            type="text"
            placeholder="Label (optional)…"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            disabled={busy}
            onKeyDown={(e) => {
              if (e.key === "Enter") void onCreate();
            }}
          />
          <button
            className="backup-primary"
            onClick={onCreate}
            disabled={busy}
          >
            {busy && !pending ? "Working…" : "Create backup now"}
          </button>
        </div>

        {error && <div className="backup-error">{error}</div>}

        {loading ? (
          <SkeletonText lines={5} className="backup-loading" />
        ) : backups.length === 0 && !error ? (
          <div className="backup-empty">
            No backups yet. Hit <strong>Create backup now</strong> to capture one.
          </div>
        ) : (
          <ul className="backup-list">
            {backups.map((b) => (
              <li
                className="backup-row"
                key={b.id}
                data-backup-id={b.id}
              >
                <div className="backup-row-main">
                  <div className="backup-row-label">{b.label}</div>
                  <div className="backup-row-meta">
                    {timeAgo(b.created_unix_ms)} · {b.file_count} files ·{" "}
                    {formatBytes(b.size_bytes)}
                  </div>
                  <code className="backup-row-id">{b.id}</code>
                </div>
                <div className="backup-row-actions">
                  <button
                    className="backup-secondary"
                    onClick={() => onRestoreClick(b)}
                    disabled={busy}
                  >
                    Restore
                  </button>
                  <button
                    className="backup-danger"
                    onClick={() => onDelete(b)}
                    disabled={busy}
                  >
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}

        {pending && (
          <div className="backup-confirm">
            <h3>Confirm restore</h3>
            <p>
              Restoring <strong>{pending.meta.label}</strong> (
              <code>{pending.meta.id}</code>) — dry run shows:
            </p>
            <ul>
              <li>
                <strong>{pending.preview.files_restored}</strong> file(s) would
                be restored
              </li>
              <li>
                <strong>{pending.preview.files_skipped}</strong> skipped (newer
                than backup, or outside allowed roots)
              </li>
              {pending.preview.errors.length > 0 && (
                <li>
                  <strong>{pending.preview.errors.length}</strong> error(s)
                  during preview
                </li>
              )}
            </ul>
            <div className="backup-confirm-actions">
              <button
                className="backup-secondary"
                onClick={() => setPending(null)}
                disabled={busy}
              >
                Cancel
              </button>
              <button
                className="backup-primary"
                onClick={onConfirmRestore}
                disabled={busy || pending.preview.files_restored === 0}
              >
                {busy ? "Restoring…" : "Confirm restore"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/** Tiny CSS.escape polyfill so the data-attribute selector survives weird ids. */
function cssEscape(s: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    return CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

// ─────────────────────────── Imperative summoner ───────────────────────────

let activeRoot: Root | null = null;

interface OpenOpts {
  focusLatest?: boolean;
}

export function openBackupPanel(opts: OpenOpts = {}): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "backup";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };

  // `/restore` wants the panel pre-focused on the most recent backup. We
  // resolve "latest" by listing once before rendering; if the list call
  // fails the panel still opens with no focus.
  if (opts.focusLatest) {
    listBackups()
      .then((items) => {
        const latest = items[0]?.id ?? null;
        root.render(<BackupPanel onClose={close} initialFocusedId={latest} />);
      })
      .catch(() => {
        root.render(<BackupPanel onClose={close} />);
      });
    return;
  }

  root.render(<BackupPanel onClose={close} />);
}
