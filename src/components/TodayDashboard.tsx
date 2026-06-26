// "Today" activity dashboard — a welcome/overview panel that aggregates the
// most actionable items into one place. Pulls focus chain, in-flight PRPs,
// recent sessions/crashes, and today's token stats into a 2-column grid of
// widget cards plus a quick-actions row.
//
// Mounted as the `"today"` ActivityPanel tab. A module-load side effect sets
// it as the default tab on first run when an active project exists; afterwards
// the user's last choice (persisted by the existing onboarding flow) wins.

import { useEffect, useMemo, useState } from "react";
import { Target, BarChart3, ListChecks, ClipboardList, MessagesSquare, Bug } from "lucide-react";

import { useCortexStore, type FocusChainTask } from "@/state/store";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { brainSnapshot, type BrainSnapshot, type RecentSession } from "@/lib/brain";
import { timeAgo } from "@/lib/time";
import { listPrps, stageOrdinal, type Prp } from "@/lib/prp";
import { recentCrashes } from "@/lib/observability";
import type { CrashRow } from "@/lib/observability";
import { usageSummary, type SessionTokens } from "@/lib/usage";
import { findCommand, makeContext } from "@/lib/slash-commands";
import { loadSessionMessages } from "@/lib/sessions";
import type { Message } from "@/state/store";

const FIRST_RUN_KEY = "cortex.todayDashboard.firstRunDone";

// Module-load side effect: on first run with an active project, surface the
// Today tab. We schedule on next tick so the store has settled (zustand
// `create()` returns synchronously but persisted fields are read at import
// time, before App's effects run).
(() => {
  if (typeof window === "undefined") return;
  let done = false;
  try {
    done = localStorage.getItem(FIRST_RUN_KEY) === "true";
  } catch {
    /* ignore */
  }
  if (done) return;
  setTimeout(() => {
    try {
      const s = useCortexStore.getState();
      // Only default to today when nothing else is selected yet AND the user
      // has a project — otherwise the existing onboarding (projects tab) wins.
      if (s.activityTab === null && s.activeProject) {
        s.setActivityTab("today");
      } else if (s.activityTab === null) {
        s.setActivityTab("projects");
      }
      localStorage.setItem(FIRST_RUN_KEY, "true");
    } catch {
      /* ignore */
    }
  }, 50);
})();

export function TodayDashboard() {
  const focusChain = useCortexStore((s) => s.focusChain);
  const runningRunIds = useCortexStore((s) => s.runningRunIds);
  const activeProject = useCortexStore((s) => s.activeProject);
  const resume = useCortexStore((s) => s.resumeSession);

  const [snap, setSnap] = useState<BrainSnapshot | null>(null);
  const [prps, setPrps] = useState<Prp[]>([]);
  const [crashes, setCrashes] = useState<CrashRow[]>([]);
  const [todayTokens, setTodayTokens] = useState(0);
  const [todayChats, setTodayChats] = useState(0);
  // When EVERY data source rejects (typically the backend is unreachable) the
  // grid would otherwise read as an all-clear empty dashboard. Surface a banner
  // so "backend down" doesn't look like "nothing happening".
  const [allDown, setAllDown] = useState(false);

  // Load every data source in parallel, then refresh on a slow interval so the
  // dashboard stays roughly fresh without thrashing the backend.
  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      const results = await Promise.allSettled([
        brainSnapshot(),
        activeProject ? listPrps(activeProject.root) : Promise.resolve([]),
        recentCrashes(20),
        usageSummary(),
      ]);
      const [snapRes, prpRes, crashRes, usageRes] = results;
      if (!mounted) return;
      if (snapRes.status === "fulfilled") setSnap(snapRes.value);
      if (prpRes.status === "fulfilled") setPrps(prpRes.value);
      if (crashRes.status === "fulfilled") setCrashes(crashRes.value);
      if (usageRes.status === "fulfilled") {
        const { chats, tokens } = todayStats(usageRes.value.by_session);
        setTodayChats(chats);
        setTodayTokens(tokens);
      }
      // Every source failed → the backend is almost certainly unreachable.
      setAllDown(results.every((r) => r.status === "rejected"));
    };
    void tick();
    const id = setInterval(tick, 30_000);
    return () => {
      mounted = false;
      clearInterval(id);
    };
  }, [activeProject]);

  const openSessions = useMemo(
    () => (snap?.recent_sessions ?? []).slice(0, 5),
    [snap],
  );
  const openCrashes = useMemo(() => crashes.slice(0, 3), [crashes]);
  const inFlightPrps = useMemo(
    () => prps.filter((p) => p.status !== "stage-4").slice(0, 5),
    [prps],
  );
  const openFocus = useMemo(
    () => focusChain.filter((t) => !t.done).slice(0, 5),
    [focusChain],
  );

  async function openSession(sessionId: string) {
    try {
      const stored = await loadSessionMessages(sessionId);
      const msgs: Message[] = stored.map((m) => ({
        id: m.id,
        role: (m.role as Message["role"]) || "assistant",
        agent: m.agent_id ?? undefined,
        content: m.content,
        reasoning: m.reasoning ?? undefined,
        pending: false,
        tools: [],
        runId: m.run_id,
      }));
      resume(sessionId, msgs);
    } catch (e) {
      pushToast({ title: "Couldn't open session", body: humanizeError(e), kind: "error" });
    }
  }

  return (
    <div className="today-dash">
      <div className="today-hero">
        <div className="today-hero-title">
          {greeting()}{activeProject ? `, working on ${activeProject.name}` : ""}.
        </div>
        <div className="today-hero-sub muted">
          {todayChats} chats · {todayTokens.toLocaleString()} tokens today
        </div>
      </div>

      <QuickActionsRow />

      {allDown && (
        <div
          className="today-empty"
          role="alert"
          style={{ color: "var(--danger)", border: "1px solid var(--danger-border)", borderRadius: "var(--radius-md)", padding: "var(--space-2) var(--space-3)" }}
        >
          Couldn't reach Cortex's backend — this dashboard may be stale or empty.
          Retrying every 30s.
        </div>
      )}

      <div className="today-grid">
        <Card title="Focus chain" icon={<Target size={15} strokeWidth={1.75} />} empty={openFocus.length === 0 ? "No open todos." : null}>
          {openFocus.map((t) => (
            <FocusRow key={t.id} task={t} />
          ))}
        </Card>

        <Card
          title="Active workflows"
          icon={<ListChecks size={15} strokeWidth={1.75} />}
          empty={runningRunIds.length === 0 ? "No active workflows." : null}
        >
          {runningRunIds.slice(0, 5).map((rid) => (
            <div key={rid} className="today-row">
              <span className="today-row-title mono">{rid.slice(-12)}</span>
              <span className="muted">running</span>
            </div>
          ))}
        </Card>

        <Card
          title="In-flight PRPs"
          icon={<ClipboardList size={15} strokeWidth={1.75} />}
          empty={inFlightPrps.length === 0 ? (activeProject ? "No staged PRPs." : "No active project.") : null}
        >
          {inFlightPrps.map((p) => (
            <button
              key={p.name}
              className="today-row clickable"
              onClick={() => useCortexStore.getState().setActivityTab("prp")}
            >
              <span className="today-row-title">{p.name}</span>
              <span className="muted">{stageOrdinal(p.status)}</span>
            </button>
          ))}
        </Card>

        <Card
          title="Recent sessions"
          icon={<MessagesSquare size={15} strokeWidth={1.75} />}
          empty={openSessions.length === 0 ? "No sessions yet." : null}
        >
          {openSessions.map((s) => (
            <SessionRow key={s.session_id} session={s} onOpen={() => void openSession(s.session_id)} />
          ))}
        </Card>

        <Card
          title="Recent crashes"
          icon={<Bug size={15} strokeWidth={1.75} />}
          empty={openCrashes.length === 0 ? "No crashes — all clear." : null}
        >
          {openCrashes.map((c) => (
            <div key={c.id} className="today-row">
              <span className="today-row-title">{truncate(c.message, 60)}</span>
              <span className="muted">{c.kind}</span>
            </div>
          ))}
        </Card>

        <Card title="Today's stats" icon={<BarChart3 size={15} strokeWidth={1.75} />} empty={null}>
          <div className="today-stat-row">
            <div className="today-stat">
              <div className="today-stat-value">{todayChats}</div>
              <div className="today-stat-label muted">chats</div>
            </div>
            <div className="today-stat">
              <div className="today-stat-value">{todayTokens.toLocaleString()}</div>
              <div className="today-stat-label muted">tokens</div>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
}

// ── Pieces ──────────────────────────────────────────────────────────────────

function Card({
  title,
  icon,
  empty,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  empty: string | null;
  children?: React.ReactNode;
}) {
  return (
    <div className="today-card">
      <div className="today-card-head">
        <span className="today-card-icon" aria-hidden="true">{icon}</span>
        <span className="today-card-title">{title}</span>
      </div>
      <div className="today-card-body">
        {empty ? <div className="muted today-empty">{empty}</div> : children}
      </div>
    </div>
  );
}

function FocusRow({ task }: { task: FocusChainTask }) {
  return (
    <div className="today-row">
      <span className="today-row-title">{task.title}</span>
    </div>
  );
}

function SessionRow({ session, onOpen }: { session: RecentSession; onOpen: () => void }) {
  const label = session.first_message ?? `session ${session.session_id.slice(-8)}`;
  return (
    <button className="today-row clickable" onClick={onOpen}>
      <span className="today-row-title">{truncate(label, 60)}</span>
      <span className="muted">{timeAgo(session.last_active_ms)}</span>
    </button>
  );
}

function QuickActionsRow() {
  const actions: { label: string; run: () => void }[] = [
    { label: "New chat", run: () => useCortexStore.getState().resetSession() },
    { label: "Open project", run: () => useCortexStore.getState().setActivityTab("projects") },
    { label: "Search", run: () => runSlash("/search") },
    { label: "Memory wizard", run: () => runSlash("/new-memory") },
    { label: "/web", run: () => runSlash("/web") },
  ];
  return (
    <div className="today-actions">
      {actions.map((a) => (
        <button key={a.label} className="today-action-btn" onClick={a.run}>
          {a.label}
        </button>
      ))}
    </div>
  );
}

// ── Helpers ─────────────────────────────────────────────────────────────────

function runSlash(input: string): void {
  const cmd = findCommand(input);
  if (!cmd) return;
  // findCommand strips the leading `/`; we already passed the no-args form so
  // pass an empty args string. The context grabs the live store.
  void cmd.run("", makeContext());
}

function todayStats(by: SessionTokens[]): { chats: number; tokens: number } {
  const start = startOfToday();
  let chats = 0;
  let tokens = 0;
  for (const s of by) {
    if (s.last_active_ms >= start) {
      chats += 1;
      tokens += s.total_tokens;
    }
  }
  return { chats, tokens };
}

function startOfToday(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

function greeting(): string {
  const h = new Date().getHours();
  if (h < 5) return "Working late";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

function truncate(s: string, max = 80): string {
  return s.length <= max ? s : `${s.slice(0, max - 1)}…`;
}

