import { AgentSidebar } from "./AgentSidebar";
import { PanelLoading } from "./Skeleton";
import { ArchitectureView, archTab, useArchTabOpen } from "./ArchitectureView";
import { ArenaPane } from "./ArenaPane";
import { BookmarksPanel } from "./BookmarksPanel";
import { BrainPanel } from "./BrainPanel";
import { ChannelsPanel } from "./ChannelsPanel";
import { CheckpointsView } from "./CheckpointsView";
import { CookbookPanel } from "./CookbookPanel";
import { DepGraphPanel } from "./DepGraphPanel";
import { EvalPanel } from "./EvalPanel";
import { FocusChain } from "./FocusChain";
import { GitHistoryPanel } from "./GitHistoryPanel";
import { HelpPanel } from "./HelpPanel";
import { GatewayCapabilitiesPanel } from "./GatewayCapabilitiesPanel";
import { KnowledgeGraph } from "./KnowledgeGraph";
import { MemoryExplorer } from "./MemoryExplorer";
import { MultiProviderPane } from "./MultiProviderPane";
import { ObservabilityPanel } from "./ObservabilityPanel";
import { OrchestratorView } from "./OrchestratorView";
import { UltimateChat } from "./UltimateChat";
import { PRPPanel } from "./PRPPanel";
import { ProjectGraph } from "./ProjectGraph";
import { ProjectMetricsPanel } from "./ProjectMetricsPanel";
import { ProjectSidebar } from "./ProjectSidebar";
import { ResearchPanel } from "./ResearchPanel";
import { RoutinesPanel } from "./RoutinesPanel";
import { SearchPanel } from "./SearchPanel";
import { SetupPanel } from "./SetupPanel";
import { SkillsPanel } from "./SkillsPanel";
import { SnippetsPanel } from "./SnippetsPanel";
import { WorkflowsPanel } from "./WorkflowsPanel";
import { SourceControlPanel } from "./SourceControlPanel";
import { TerminalPane } from "./TerminalPane";
import { ThreadsList } from "./ThreadsList";
import { TodayDashboard } from "./TodayDashboard";
import { ToolsRegistryPanel } from "./ToolsRegistryPanel";
import { TrustMatrix } from "./TrustMatrix";
import { UsageView } from "./UsageView";
import { WebPreviewPane } from "./WebPreviewPane";
import { useCortexStore } from "@/state/store";
import { ActivityIcon, ARCHITECTURE_ICON } from "@/lib/activity-icons";
import { tabTitle } from "@/lib/activity-tabs";
import { timeAgo } from "@/lib/time";
import { brainSnapshot, type BrainSnapshot, type RecentSession } from "@/lib/brain";
import { bootstrapProjectSession, loadSessionMessages } from "@/lib/sessions";
import { searchSessions, type SessionSearchHit } from "@/lib/session-search";
import { setActiveProject } from "@/lib/projects";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { lazy, Suspense, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { ActivityTab, Message } from "@/state/store";

// Code-split the two CodeMirror-backed panels out of the main bundle. They're
// the heaviest static dependency (the editor view/search/lang stack) yet are
// only mounted when their tab is active, so the editor chunk need only load on
// first open. Named exports, so map them to a default for React.lazy.
const EditorPane = lazy(() =>
  import("./EditorPane").then((m) => ({ default: m.EditorPane })),
);
const MultiBuffer = lazy(() =>
  import("./MultiBuffer").then((m) => ({ default: m.MultiBuffer })),
);

// Tabs that hold live, unrecoverable work: an open PTY, unsaved CodeMirror
// buffers. Once visited they stay MOUNTED for the app's lifetime and are
// CSS-hidden when another tab (or no tab) is active — unmounting them is what
// used to silently kill the shell and discard unsaved edits on a mere glance
// at another tab. Everything else keeps the cheap conditional-mount behavior.
const KEEP_ALIVE_TABS = ["editor", "multibuffer", "terminal"] as const;
type KeepAliveTab = (typeof KEEP_ALIVE_TABS)[number];

function isKeepAliveTab(t: ActivityTab): t is KeepAliveTab {
  return (KEEP_ALIVE_TABS as readonly string[]).includes(t as string);
}

/** Mounted-but-hidden wrapper for a keep-alive surface. `display:none` (not
 *  visibility) so the hidden pane takes no layout; xterm/CodeMirror re-measure
 *  on re-show via their own ResizeObservers. */
function KeepAlive({ active, children }: { active: boolean; children: ReactNode }) {
  return (
    <div className="activity-keepalive" style={active ? undefined : { display: "none" }}>
      {children}
    </div>
  );
}

export function ActivityPanel() {
  const tab = useCortexStore((s) => s.activityTab);
  const archOpen = useArchTabOpen();

  // Keep-alive tabs the user has opened at least once this session. A ref
  // mutated during render (monotonic grow-only set) — the re-render that
  // matters is always driven by the `tab` store change itself.
  const visitedRef = useRef<Set<KeepAliveTab>>(new Set());
  if (!archOpen && tab && isKeepAliveTab(tab)) visitedRef.current.add(tab);
  const visited = visitedRef.current;

  // Mirrors App.tsx's grid logic: the panel column exists for the arch tab or
  // any union tab except "projects" (which lives in the persistent sidebar).
  const open = archOpen || (tab !== null && tab !== "projects");

  // Fast path — nothing kept alive and the panel is closed: render nothing,
  // exactly the pre-keep-alive behavior.
  if (!open && visited.size === 0) return null;

  // The architecture surface is a NEW tab whose id is intentionally outside
  // the `ActivityTab` union (owned elsewhere this wave); it has its own open
  // flag and takes over the body while open. Keep-alive panes stay mounted
  // (hidden) underneath it so an arch detour can't kill the shell either.
  const showTab = open && !archOpen ? tab : null;

  return (
    <div className="activity-panel" style={open ? undefined : { display: "none" }}>
      <div className="activity-panel-head">
        {archOpen ? (
          <>
            <span className="activity-tab-pill active" title="Architecture">
              <span className="icon" aria-hidden="true">
                <ARCHITECTURE_ICON size={16} strokeWidth={1.75} />
              </span>
              <span className="label">Architecture</span>
            </span>
            <button className="link-btn" onClick={() => archTab.close()}>×</button>
          </>
        ) : tab ? (
          <>
            <span className={`activity-tab-pill active`} title={labelFor(tab)}>
              <span className="icon" aria-hidden="true"><ActivityIcon tab={tab} /></span>
              <span className="label">{labelFor(tab)}</span>
            </span>
            <button className="link-btn" onClick={() => useCortexStore.getState().setActivityTab(null)}>×</button>
          </>
        ) : null}
      </div>
      <div className="activity-panel-body">
        {visited.has("editor") && (
          <KeepAlive active={showTab === "editor"}>
            <Suspense fallback={<PanelLoading />}><EditorPane /></Suspense>
          </KeepAlive>
        )}
        {visited.has("multibuffer") && (
          <KeepAlive active={showTab === "multibuffer"}>
            <Suspense fallback={<PanelLoading />}><MultiBuffer /></Suspense>
          </KeepAlive>
        )}
        {visited.has("terminal") && (
          <KeepAlive active={showTab === "terminal"}>
            <TerminalPane />
          </KeepAlive>
        )}
        {archOpen && <ArchitectureView />}
        {showTab === "today" && <TodayDashboard />}
        {showTab === "brain" && <BrainPanel />}
        {showTab === "memory" && <MemoryExplorer />}
        {showTab === "sessions" && <SessionsList />}
        {showTab === "projects" && <ProjectSidebar />}
        {showTab === "graph" && <ProjectGraph />}
        {showTab === "agents" && <AgentSidebar />}
        {showTab === "usage" && <UsageView />}
        {showTab === "observability" && <ObservabilityPanel />}
        {showTab === "checkpoints" && <CheckpointsView />}
        {showTab === "threads" && <ThreadsList />}
        {showTab === "focus" && <FocusChain />}
        {showTab === "trust" && <TrustMatrix />}
        {showTab === "skills" && <SkillsPanel />}
        {showTab === "prp" && <PRPPanel />}
        {showTab === "git" && <GitHistoryPanel />}
        {showTab === "source-control" && <SourceControlPanel />}
        {showTab === "preview" && <WebPreviewPane />}
        {showTab === "orchestrator" && <OrchestratorView />}
        {showTab === "ultimate" && <UltimateChat />}
        {showTab === "tools" && <ToolsRegistryPanel />}
        {showTab === "snippets" && <SnippetsPanel />}
        {showTab === "workflows" && <WorkflowsPanel />}
        {showTab === "help" && <HelpPanel />}
        {showTab === "search" && <SearchPanel />}
        {showTab === "gateway" && <GatewayCapabilitiesPanel />}
        {showTab === "knowledge-graph" && <KnowledgeGraph />}
        {showTab === "dep-graph" && <DepGraphPanel />}
        {showTab === "metrics" && <ProjectMetricsPanel />}
        {showTab === "bookmarks" && <BookmarksPanel />}
        {showTab === "arena" && <ArenaPane />}
        {showTab === "channels" && <ChannelsPanel />}
        {showTab === "lanes" && <MultiProviderPane />}
        {showTab === "cookbook" && <CookbookPanel />}
        {showTab === "research" && <ResearchPanel />}
        {showTab === "routines" && <RoutinesPanel />}
        {showTab === "eval" && <EvalPanel />}
        {showTab === "setup" && <SetupPanel />}
      </div>
    </div>
  );
}

// Panel-header title. Sourced from the single tab registry (lib/activity-tabs)
// so the header can't drift from the rail/palette labels.
function labelFor(tab: NonNullable<ReturnType<typeof useCortexStore.getState>["activityTab"]>): string {
  return tabTitle(tab);
}

function SessionsList() {
  const [snap, setSnap] = useState<BrainSnapshot | null>(null);
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [hits, setHits] = useState<SessionSearchHit[]>([]);
  const [searching, setSearching] = useState(false);
  /** brainSnapshot failed — without this the list shows a skeleton forever
   *  (or silently stale data) and a backend error reads as "no sessions". */
  const [loadError, setLoadError] = useState<string | null>(null);
  /** The last clicked session failed to resume. */
  const [resumeError, setResumeError] = useState<string | null>(null);
  const resume = useCortexStore((s) => s.resumeSession);

  useEffect(() => {
    let mounted = true;
    // Toast at most once per failure streak so the 8s poll can't spam on a
    // backend hiccup; reset the latch once a snapshot succeeds again.
    let warned = false;
    const tick = async () => {
      try {
        const s = await brainSnapshot();
        if (!mounted) return;
        setSnap(s);
        setLoadError(null);
        warned = false;
      } catch (err) {
        console.warn("sessions snapshot load failed", err);
        if (!mounted) return;
        setLoadError(humanizeError(err));
        if (!warned) {
          warned = true;
          pushToast({
            title: "Couldn't load sessions",
            body: "The sessions list may be out of date.",
            kind: "error",
          });
        }
      }
    };
    void tick();
    const id = setInterval(tick, 8_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  // 300ms debounce on the search input
  useEffect(() => {
    const id = setTimeout(() => setDebounced(query.trim()), 300);
    return () => clearTimeout(id);
  }, [query]);

  useEffect(() => {
    let cancelled = false;
    if (!debounced) { setHits([]); setSearching(false); return; }
    setSearching(true);
    searchSessions(debounced, 50)
      .then((r) => { if (!cancelled) setHits(r); })
      .catch(() => { if (!cancelled) setHits([]); })
      .finally(() => { if (!cancelled) setSearching(false); });
    return () => { cancelled = true; };
  }, [debounced]);

  async function openSession(sessionId: string) {
    setResumeError(null);
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
    } catch (err) {
      console.warn("session resume failed", err);
      // Inline state + toast — a failed resume must not look like a no-op.
      setResumeError(humanizeError(err));
      pushToast({
        title: "Couldn't open session",
        body: humanizeError(err),
        kind: "error",
      });
    }
  }

  const searchMode = debounced.length > 0;

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div style={{ padding: "var(--space-2)", borderBottom: "1px solid var(--border)" }}>
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search messages…"
          style={{
            width: "100%",
            padding: "6px var(--space-2)",
            background: "var(--bg-elev)",
            color: "var(--text)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            fontSize: "var(--text-sm)",
          }}
        />
      </div>
      <div style={{ flex: 1, overflow: "auto" }}>
        {resumeError && (
          <div style={{ padding: "var(--space-2) var(--space-2) 0" }}>
            <div className="session-picker-error" role="alert" style={{ margin: 0 }}>
              <strong>Couldn't open that session.</strong> {resumeError} Pick
              another session, or try again.
            </div>
          </div>
        )}
        {searchMode
          ? <SearchHits hits={hits} query={debounced} loading={searching} onOpen={openSession} />
          : <RecentList snap={snap} error={loadError} onOpen={(s) => void openSession(s.session_id)} />}
      </div>
    </div>
  );
}

function RecentList({
  snap,
  error,
  onOpen,
}: {
  snap: BrainSnapshot | null;
  error: string | null;
  onOpen: (s: RecentSession) => void;
}) {
  // Never got a snapshot AND the backend is erroring: show a real error row
  // instead of an eternal loading skeleton or a fake "No sessions yet."
  if (!snap && error) {
    return (
      <div style={{ padding: "var(--space-2)" }}>
        <div className="session-picker-error" role="alert" style={{ margin: 0 }}>
          <strong>Couldn't load your sessions.</strong> {error}
        </div>
      </div>
    );
  }
  if (!snap) return <PanelLoading />;
  if (snap.recent_sessions.length === 0) {
    return (
      <div className="muted" style={{ padding: "var(--space-4)", textAlign: "center" }}>
        No sessions yet.
      </div>
    );
  }
  return (
    <div className="brain-list" style={{ padding: "var(--space-2)" }}>
      {snap.recent_sessions.map((s) => (
        <button key={s.session_id} className="brain-row clickable" onClick={() => onOpen(s)}>
          <div className="brain-row-head">
            <strong>{s.first_message ?? `session ${s.session_id.slice(-8)}`}</strong>
            <span className="muted">{timeAgo(s.last_active_ms)}</span>
          </div>
          <div className="brain-meta">
            {s.message_count} msgs · {s.agents.filter(Boolean).join(", ") || "—"}
          </div>
        </button>
      ))}
    </div>
  );
}

function SearchHits({
  hits,
  query,
  loading,
  onOpen,
}: {
  hits: SessionSearchHit[];
  query: string;
  loading: boolean;
  onOpen: (sessionId: string) => void;
}) {
  if (loading && hits.length === 0) {
    return <div className="muted" style={{ padding: "var(--space-3)" }}>searching…</div>;
  }
  if (hits.length === 0) {
    return (
      <div className="muted" style={{ padding: "var(--space-4)", textAlign: "center" }}>
        No matches.
      </div>
    );
  }
  return (
    <div className="brain-list" style={{ padding: "var(--space-2)" }}>
      {hits.map((h, i) => (
        <button
          key={`${h.session_id}-${h.ts}-${i}`}
          className="brain-row clickable"
          onClick={() => onOpen(h.session_id)}
        >
          <div className="brain-row-head">
            <span className={`role-badge role-${h.role}`}>{h.role}</span>
            <span className="muted">{timeAgo(h.ts)}</span>
          </div>
          <HighlightedSnippet snippet={h.snippet} query={query} />
          <div className="brain-meta">
            {/* Mono session-id microlabel — floored at --text-xs per DESIGN-SPEC §3. */}
            <code style={{ fontFamily: "var(--mono, monospace)", fontSize: "var(--text-xs)" }}>
              {h.session_id.slice(-12)}
            </code>
          </div>
        </button>
      ))}
    </div>
  );
}

function HighlightedSnippet({ snippet, query }: { snippet: string; query: string }) {
  const parts = useMemo(() => splitHighlight(snippet, query), [snippet, query]);
  return (
    <div
      style={{
        fontSize: "var(--text-sm)",
        lineHeight: "var(--leading-snug)",
        margin: "var(--space-0_5) 0",
        color: "var(--text)",
      }}
    >
      {parts.map((p, i) =>
        p.match ? <mark key={i}>{p.text}</mark> : <span key={i}>{p.text}</span>
      )}
    </div>
  );
}

function splitHighlight(text: string, query: string): { text: string; match: boolean }[] {
  if (!query) return [{ text, match: false }];
  const out: { text: string; match: boolean }[] = [];
  const lcText = text.toLowerCase();
  const lcQuery = query.toLowerCase();
  let i = 0;
  while (i < text.length) {
    const j = lcText.indexOf(lcQuery, i);
    if (j === -1) {
      out.push({ text: text.slice(i), match: false });
      break;
    }
    if (j > i) out.push({ text: text.slice(i, j), match: false });
    out.push({ text: text.slice(j, j + lcQuery.length), match: true });
    i = j + lcQuery.length;
  }
  return out;
}

// Avoid unused warnings — ProjectSidebar is used above.
export { setActiveProject as _unused, bootstrapProjectSession as _unused2 };
