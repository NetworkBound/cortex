import { useEffect, useState } from "react";
import { Bell, Brain, Loader2 } from "lucide-react";
import { homelabHealth, type HealthRow } from "@/lib/observability";
import { listMemoryFiles } from "@/lib/memory";
import { SandboxBadge } from "@/components/SandboxBadge";
import { RepoWatchBadge } from "@/components/RepoWatchBadge";
import { ModelStrip } from "@/components/ModelStrip";
import { TokenHUD } from "@/components/TokenHUD";
import { useCortexStore } from "@/state/store";
import { initJobStore, useJobs, JOB_TAB } from "@/state/jobs";
import { initRoutineNotifications } from "@/lib/routines";
import { startRepoWatcher, stopRepoWatcher } from "@/lib/repo-watcher";
import {
  activateNotificationCenter,
  openNotificationCenter,
  useUnread,
} from "@/lib/notification-center";

export function StatusBar() {
  const [health, setHealth] = useState<HealthRow[]>([]);
  const activeProject = useCortexStore((s) => s.activeProject);
  const sessionId = useCortexStore((s) => s.sessionId);
  const runningRunIds = useCortexStore((s) => s.runningRunIds);
  const messages = useCortexStore((s) => s.messages);
  const hasApiKey = useCortexStore((s) => s.hasApiKey);
  const currentMode = useCortexStore((s) => s.currentMode);
  const setCurrentMode = useCortexStore((s) => s.setCurrentMode);
  const statusBarCompact = useCortexStore((s) => s.statusBarCompact);

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const h = await homelabHealth();
        if (mounted) setHealth(h);
      } catch { /* backend warming */ }
    };
    void tick();
    const id = setInterval(tick, 10_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  // Auto-start the repo watcher when the active project changes so the
  // RepoWatchBadge has a live event stream to subscribe to.
  useEffect(() => {
    if (!activeProject) return;
    const root = activeProject.root;
    void startRepoWatcher(root).catch(() => { /* non-fatal */ });
    return () => {
      void stopRepoWatcher(root).catch(() => { /* non-fatal */ });
    };
  }, [activeProject]);

  // Notification center: keep streams + pull-refresh hot for the lifetime of
  // the StatusBar so the bell badge reflects fresh state even before the modal
  // is ever opened.
  useEffect(() => activateNotificationCenter(), []);
  const unread = useUnread();

  // Global job store boot: re-adopt any backend-side in-flight work (e.g. a
  // model pull surviving a webview reload). Idempotent.
  useEffect(() => initJobStore(), []);

  // Routine outcomes → NotificationCenter from any tab (scheduled failures
  // also toast). Idempotent, same always-mounted rationale as initJobStore.
  useEffect(() => initRoutineNotifications(), []);

  const okCount = health.filter((h) => h.ok).length;
  const totalCount = health.length;

  return (
    <div className={`status-bar${statusBarCompact ? " compact" : ""}`}>
      <div className="status-left">
        <button
          type="button"
          className={`status-pill mode-toggle mode-${currentMode}`}
          onClick={() => setCurrentMode(currentMode === "plan" ? "act" : "plan")}
          title={
            currentMode === "plan"
              ? "Plan mode: write/exec tools blocked. Click to switch to Act."
              : "Act mode: tools enabled. Click to switch to Plan."
          }
        >
          {currentMode === "plan" ? "▷ PLAN" : "▶ ACT"}
        </button>
        {!statusBarCompact && (
          <span className={`status-pill ${hasApiKey ? "ok" : "warn"}`}>
            {hasApiKey ? "Gateway connected" : "no API key"}
          </span>
        )}
        {!statusBarCompact && activeProject && (
          <span className="status-pill">
            ▸ {activeProject.name}
          </span>
        )}
        <SandboxBadge />
        {!statusBarCompact && <RepoWatchBadge />}
        {runningRunIds.length > 0 && (
          <span className="status-pill running">
            <span className="dot ok" /> {runningRunIds.length} running
          </span>
        )}
        <JobsPill />
      </div>
      <div className="status-center">
        {totalCount > 0 && (
          <span className="status-pill" title="Configured service health">
            services {okCount}/{totalCount} up
          </span>
        )}
        <BrainCountPill />
        <BrainAutoPill />
      </div>
      {!statusBarCompact && (
        <div className="status-models">
          <ModelStrip />
        </div>
      )}
      <div className="status-right">
        {unread.count > 0 && (
          <button
            type="button"
            className={`status-pill notif-badge sev-${unread.severity ?? "info"}`}
            onClick={() => void openNotificationCenter()}
            title={`${unread.count} unread notification${unread.count === 1 ? "" : "s"}`}
          >
            <Bell size={14} strokeWidth={1.75} aria-hidden="true" /> {unread.count}
          </button>
        )}
        <TokenHUD />
        {!statusBarCompact && (
          <span className="status-pill subtle">{messages.length} msgs</span>
        )}
        {!statusBarCompact && (
          <span className="status-pill subtle">session {sessionId.slice(-8)}</span>
        )}
      </div>
    </div>
  );
}

/**
 * Live indicator for long-running jobs (Cookbook pulls, Deep Research, Eval).
 * Reads the global job store, so it stays accurate no matter which tab is
 * open — the StatusBar is the one surface that never unmounts. One job shows
 * its label + percent; several collapse to a count. Click jumps to the tab
 * that owns the most recent job. Stays visible in compact mode: in-flight
 * work is exactly what a status bar must never hide.
 */
function JobsPill() {
  const jobs = useJobs((s) => s.jobs);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const running = Object.values(jobs).sort((a, b) => b.startedAt - a.startedAt);
  if (running.length === 0) return null;
  const head = running[0];
  const label =
    running.length === 1
      ? `${head.label}${head.pct != null ? ` ${Math.round(head.pct)}%` : ""}`
      : `${running.length} jobs running`;
  return (
    <button
      type="button"
      className="status-pill jobs-pill"
      onClick={() => setActivityTab(JOB_TAB[head.kind])}
      title={running.map((j) => `${j.label} — ${j.detail}`).join("\n")}
    >
      <Loader2 size={13} strokeWidth={2} className="jobs-pill-spinner" aria-hidden="true" />
      {label}
    </button>
  );
}

/**
 * Brain "N memories" pill. Polls `list_memory_files` every 60s — slow enough
 * to not pressure the WSL UNC scan. Hidden when count is 0 so first-launch
 * users (before WSL paths probed) don't see a "0".
 */
function BrainAutoPill() {
  const enabled = useCortexStore((s) => s.brainAutoEnabled);
  const set = useCortexStore((s) => s.setBrainAutoEnabled);
  return (
    <button
      type="button"
      className={`status-pill brain-auto ${enabled ? "on" : "off"}`}
      onClick={() => set(!enabled)}
      title={`Brain auto-trigger ${enabled ? "ON — click to disable" : "OFF — click to enable"}. Or use /brain ${enabled ? "off" : "on"}. Implicit path mentions auto-attach regardless.`}
    >
      <Brain size={14} strokeWidth={1.75} aria-hidden="true" /> {enabled ? "auto" : "off"}
    </button>
  );
}

function BrainCountPill() {
  const [count, setCount] = useState<number | null>(null);
  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const files = await listMemoryFiles(undefined, undefined);
        if (mounted) setCount(files.length);
      } catch { /* ignore */ }
    };
    void tick();
    const id = setInterval(tick, 60_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);
  if (count == null || count === 0) return null;
  return (
    <span
      className="status-pill brain-count"
      title={`Brain indexes ${count} memory files (Auto-memory + project + global + Obsidian + runbooks)`}
    >
      <Brain size={14} strokeWidth={1.75} aria-hidden="true" /> {count}
    </span>
  );
}
