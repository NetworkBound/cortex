import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { SkeletonText } from "./Skeleton";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import {
  formatBytes,
  getSettings,
  runBackupNow,
  setSettings,
  timeAgo,
  type BackupReport,
  type GiteaSettings,
} from "@/lib/gitea-backup";
import { pushToast } from "@/lib/toast";

/**
 * Settings + status panel for the Gitea backup auto-mirror (`/gitea-backup`).
 * Self-mounting portal — same pattern as StashManagerModal / DepAuditModal
 * so App.tsx stays untouched.
 *
 * Layout:
 *   - Header form: base URL, token, owner, repo, enabled toggle.
 *   - Status row: last-backup time + Open repo button.
 *   - Backup-now button → runs immediately, shows live BackupReport.
 *   - Last-report block (added/changed/deleted/bytes/errors).
 */

interface GiteaBackupPanelProps {
  onClose: () => void;
}

const EMPTY: GiteaSettings = {
  enabled: false,
  base_url: "",
  token: "",
  owner: "",
  repo: "",
  last_backup_unix_ms: 0,
  last_report: null,
};

export function GiteaBackupPanel({ onClose }: GiteaBackupPanelProps) {
  const [settings, setLocal] = useState<GiteaSettings>(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<BackupReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  // ESC closes the modal — matches every other transient surface in Cortex.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Initial settings load. Failures fall back to defaults so the form is
  // always interactive even on a totally fresh install.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const s = await getSettings();
      if (cancelled) return;
      setLocal(s);
      setReport(s.last_report ?? null);
      setLoaded(true);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const update = useCallback(
    <K extends keyof GiteaSettings>(key: K, value: GiteaSettings[K]) => {
      setLocal((prev) => ({ ...prev, [key]: value }));
    },
    [],
  );

  const onSave = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await setSettings(settings);
      pushToast({
        title: "Gitea backup",
        body: "Settings saved",
        kind: "success",
      });
    } catch (e) {
      const msg = humanizeError(e);
      setError(msg);
      pushToast({ title: "Save failed", body: msg, kind: "error" });
    } finally {
      setBusy(false);
    }
  }, [settings]);

  const onBackupNow = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      // Persist first so the scheduler + this run agree on credentials.
      await setSettings(settings);
      const r = await runBackupNow();
      setReport(r);
      const ok = r.errors.length === 0;
      pushToast({
        title: ok ? "Backup complete" : "Backup finished with errors",
        body: ok
          ? `${r.commits_made > 0 ? "Pushed " : "No changes — "}${r.files_added}+ ${r.files_changed}~ ${r.files_deleted}-`
          : r.errors[0] ?? "see panel for details",
        kind: ok ? "success" : "error",
      });
    } catch (e) {
      const msg = humanizeError(e);
      setError(msg);
      pushToast({ title: "Backup failed", body: msg, kind: "error" });
    } finally {
      setBusy(false);
    }
  }, [settings]);

  const onOpenRepo = useCallback(async () => {
    const url = report?.repo_url || webUrlFor(settings);
    if (!url) return;
    try {
      await shellOpen(url);
    } catch (e) {
      pushToast({ title: "Couldn't open", body: humanizeError(e), kind: "error" });
    }
  }, [report, settings]);

  const canBackup =
    settings.base_url.trim() !== "" &&
    settings.token.trim() !== "" &&
    settings.owner.trim() !== "" &&
    settings.repo.trim() !== "";

  return (
    <div className="gitea-backup-overlay" onClick={onClose}>
      <div className="gitea-backup-modal" onClick={(e) => e.stopPropagation()}>
        <header className="gitea-backup-header">
          <h2>Gitea backup</h2>
          <button className="gitea-backup-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        {!loaded ? (
          <SkeletonText lines={6} className="gitea-backup-loading" />
        ) : (
          <>
            <section className="gitea-backup-form">
              <label className="gitea-backup-field">
                <span>Base URL</span>
                <input
                  type="text"
                  placeholder="https://gitea.local:3000"
                  value={settings.base_url}
                  onChange={(e) => update("base_url", e.target.value)}
                  disabled={busy}
                />
              </label>
              <label className="gitea-backup-field">
                <span>Token</span>
                <input
                  type="password"
                  placeholder="gitea personal access token"
                  value={settings.token}
                  onChange={(e) => update("token", e.target.value)}
                  disabled={busy}
                  autoComplete="off"
                />
              </label>
              <div className="gitea-backup-row">
                <label className="gitea-backup-field">
                  <span>Owner</span>
                  <input
                    type="text"
                    placeholder="user"
                    value={settings.owner}
                    onChange={(e) => update("owner", e.target.value)}
                    disabled={busy}
                  />
                </label>
                <label className="gitea-backup-field">
                  <span>Repo</span>
                  <input
                    type="text"
                    placeholder="cortex-backup"
                    value={settings.repo}
                    onChange={(e) => update("repo", e.target.value)}
                    disabled={busy}
                  />
                </label>
              </div>
              <label className="gitea-backup-toggle">
                <input
                  type="checkbox"
                  checked={settings.enabled}
                  onChange={(e) => update("enabled", e.target.checked)}
                  disabled={busy}
                />
                <span>Auto-backup every 6 hours</span>
              </label>
            </section>

            <section className="gitea-backup-status">
              <div className="gitea-backup-status-line">
                <span>Last backup:</span>
                <strong>{timeAgo(settings.last_backup_unix_ms)}</strong>
                <button
                  className="gitea-backup-secondary"
                  onClick={() => void onOpenRepo()}
                  disabled={!canBackup}
                >
                  Open repo
                </button>
              </div>
            </section>

            {error && <div className="gitea-backup-error">{error}</div>}

            {report && (
              <section className="gitea-backup-report">
                <h3>Most recent run</h3>
                <ul>
                  <li>Commits: {report.commits_made}</li>
                  <li>Added: {report.files_added}</li>
                  <li>Changed: {report.files_changed}</li>
                  <li>Deleted: {report.files_deleted}</li>
                  <li>Payload: {formatBytes(report.bytes_total)}</li>
                  {report.errors.length > 0 && (
                    <li className="gitea-backup-errors">
                      Errors:
                      <ul>
                        {report.errors.slice(0, 5).map((e, i) => (
                          <li key={i}>{e}</li>
                        ))}
                        {report.errors.length > 5 && (
                          <li>… +{report.errors.length - 5} more</li>
                        )}
                      </ul>
                    </li>
                  )}
                </ul>
              </section>
            )}

            <footer className="gitea-backup-footer">
              <button
                className="gitea-backup-primary"
                onClick={() => void onBackupNow()}
                disabled={busy || !canBackup}
              >
                {busy ? "Working…" : "Backup now"}
              </button>
              <button
                className="gitea-backup-secondary"
                onClick={() => void onSave()}
                disabled={busy}
              >
                Save settings
              </button>
              <button className="gitea-backup-secondary" onClick={onClose}>
                Close
              </button>
            </footer>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * Best-effort web URL when there's no report yet — strips the credential
 * shape from the configured base URL and tacks on `<owner>/<repo>`.
 */
function webUrlFor(s: GiteaSettings): string {
  if (!s.base_url || !s.owner || !s.repo) return "";
  const base = s.base_url.replace(/\/+$/, "");
  return `${base}/${s.owner}/${s.repo}`;
}

let activeRoot: Root | null = null;

export function openGiteaBackupPanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "gitea-backup";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<GiteaBackupPanel onClose={close} />);
}
