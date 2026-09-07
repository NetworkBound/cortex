import { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  activateNotificationCenter,
  isRead,
  markAllRead,
  openNotification,
  refreshNotificationCenter,
  useNotifications,
  type NotifFilter,
  type Notification,
  type NotifSeverity,
} from "@/lib/notification-center";
import { timeAgo } from "@/lib/time";

/**
 * Unified notification inbox modal. Self-mounting portal — same pattern as
 * AuditLogPanel / CrashViewer so App.tsx stays untouched.
 *
 * Filter chips, "Mark all read", and per-row click → deep-link dispatch are
 * all wired through `@/lib/notification-center`; this file is pure view.
 */

interface NotificationCenterProps {
  onClose: () => void;
}

const FILTERS: { id: NotifFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "errors", label: "Errors" },
  { id: "warnings", label: "Warnings" },
  { id: "job", label: "Jobs" },
  { id: "audit", label: "Audit" },
  { id: "monitor", label: "Monitor" },
  { id: "config", label: "Config" },
  { id: "repo", label: "Repo" },
];

function severityIcon(sev: NotifSeverity): string {
  if (sev === "error") return "✖";
  if (sev === "warning") return "▲";
  return "•";
}

function matchesFilter(n: Notification, filter: NotifFilter): boolean {
  if (filter === "all") return true;
  if (filter === "errors") return n.severity === "error";
  if (filter === "warnings") return n.severity === "warning";
  return n.source === filter;
}

export function NotificationCenter({ onClose }: NotificationCenterProps) {
  const [filter, setFilter] = useState<NotifFilter>("all");
  const notifications = useNotifications();

  // Activate pull-refresh + push-streams while the modal is mounted.
  useEffect(() => activateNotificationCenter(), []);

  // 5-second auto-refresh is provided by activateNotificationCenter's internal
  // interval; this just re-pings the backend so newly-arrived rows surface a
  // tick sooner than the next scheduled pull (e.g. after the user clicks a
  // chip).
  useEffect(() => {
    void refreshNotificationCenter();
  }, [filter]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const visible = useMemo(
    () => notifications.filter((n) => matchesFilter(n, filter)),
    [notifications, filter],
  );

  // Per-chip counts (against the full set, not the filtered slice — chips
  // need to show the absolute totals).
  const counts = useMemo(() => {
    const c: Record<NotifFilter, number> = {
      all: notifications.length,
      errors: 0,
      warnings: 0,
      job: 0,
      audit: 0,
      monitor: 0,
      config: 0,
      repo: 0,
      crash: 0,
      issue: 0,
    };
    for (const n of notifications) {
      if (n.severity === "error") c.errors += 1;
      if (n.severity === "warning") c.warnings += 1;
      c[n.source] += 1;
    }
    return c;
  }, [notifications]);

  const onRowClick = useCallback(async (n: Notification) => {
    await openNotification(n);
  }, []);

  return (
    <div className="notif-backdrop" onMouseDown={onClose}>
      <div
        className="notif-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="notif-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="notif-header">
          <h2 id="notif-title">Notifications</h2>
          <div className="notif-header-actions">
            <button
              className="notif-mark-read"
              onClick={() => markAllRead()}
              disabled={visible.every((n) => isRead(n.id))}
            >
              Mark all read
            </button>
            <button className="notif-close" onClick={onClose} aria-label="Close">
              ×
            </button>
          </div>
        </header>

        <p className="notif-summary">
          Aggregated inbox — finished jobs, crashes, issues, audit, monitors,
          config + repo changes. Click a row to jump to its viewer.
        </p>

        <section className="notif-filters">
          {FILTERS.map((f) => (
            <button
              key={f.id}
              type="button"
              className={`notif-chip${filter === f.id ? " active" : ""}`}
              onClick={() => setFilter(f.id)}
            >
              {f.label}
              <span className="notif-chip-count">{counts[f.id] ?? 0}</span>
            </button>
          ))}
        </section>

        <div className="notif-list">
          {visible.length === 0 ? (
            <div className="notif-empty">
              {notifications.length === 0
                ? "No notifications yet."
                : "Nothing matches the current filter."}
            </div>
          ) : (
            visible.map((n) => {
              const read = isRead(n.id);
              return (
                <button
                  key={n.id}
                  type="button"
                  className={`notif-row notif-sev-${n.severity}${read ? " read" : ""}`}
                  onClick={() => void onRowClick(n)}
                  title={n.detail ?? n.message}
                >
                  <span className="notif-sev-icon" aria-hidden>
                    {severityIcon(n.severity)}
                  </span>
                  <span className="notif-source">{n.source}</span>
                  <span className="notif-message">{n.message}</span>
                  <span className="notif-ts">{timeAgo(n.ts, { absoluteAfterDays: 30 })}</span>
                </button>
              );
            })
          )}
        </div>

        <footer className="notif-footer">
          <span className="notif-status">
            {visible.length} shown · {notifications.length} total · auto-refresh 5s
          </span>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner — mounts a detached React root on document.body and
 * tears it down on close. Mirrors `openAuditLogPanel` so the slash command
 * and the StatusBar badge can summon the same modal.
 */
let activeRoot: Root | null = null;

export function mountNotificationCenter(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "notification-center";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<NotificationCenter onClose={close} />);
}
