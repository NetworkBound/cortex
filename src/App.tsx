import { useEffect, useState } from "react";
import { ActivityBar } from "./components/ActivityBar";
import { AutoUpdater } from "./components/AutoUpdater";
import { ActivityPanel } from "./components/ActivityPanel";
import { useArchTabOpen } from "./components/ArchitectureView";
import { TrustBanner } from "./components/TrustBanner";
import { AgentSidebar } from "./components/AgentSidebar";
import { BrainPanel } from "./components/BrainPanel";
import { ChatHistorySidebar } from "./components/ChatHistorySidebar";
import { ChatPane } from "./components/ChatPane";
import { CommandPalette } from "./components/CommandPalette";
import { MemoryExplorer } from "./components/MemoryExplorer";
import { ObservabilityPanel } from "./components/ObservabilityPanel";
import { OnboardingTour } from "./components/OnboardingTour";
import { OnboardingWizard } from "./components/OnboardingWizard";
import { ProjectSidebar } from "./components/ProjectSidebar";
import { SessionPicker } from "./components/SessionPicker";
import { SidebarResizer } from "./components/SidebarResizer";
import { SettingsModal } from "./components/SettingsModal";
import { ShortcutsModal } from "./components/ShortcutsModal";
import { StatusBar } from "./components/StatusBar";
import { SurfaceLayer } from "./components/SurfaceLayer";
import { ToastRack } from "./components/ToastRack";
import { DialogHost } from "./components/DialogHost";
import { CheckpointReviewHost } from "./components/CheckpointReviewHost";
import { getGatewayConfig } from "./lib/cortex-bridge";
import { DEFAULT_KEYMAP, matchCombo } from "./lib/keymap";
import { subscribeMonitorLines } from "./lib/monitors";
import { useThemeBoot } from "./lib/use-theme-boot";
import { useE2EProbe } from "./lib/e2e-probe";
import { useAutoCondense } from "./lib/auto-condense";
import { attachUIStatePersistence, loadUIState } from "./lib/ui-persistence";
import { attachPrefMirror } from "./lib/pref-sync";
import { useCortexStore, type ActivityTab } from "./state/store";
import { pushToast } from "./lib/toast";

type RightTab = "agent" | "brain" | "memory" | "chats";

// Display order for `Ctrl+Tab` cycling. Kept in sync manually with the
// `ActivityTab` union in `state/store.ts`; we deliberately *don't* extract
// it at runtime because the union is a TS-only construct. The order here
// mirrors the union's declared order so the cycle feels predictable.
const ACTIVITY_TAB_ORDER: readonly NonNullable<ActivityTab>[] = [
  "brain",
  "memory",
  "sessions",
  "projects",
  "graph",
  "agents",
  "usage",
  "observability",
  "checkpoints",
  "threads",
  "focus",
  "trust",
  "skills",
  "prp",
  "terminal",
  "git",
  "source-control",
  "editor",
  "preview",
  "orchestrator",
  "tools",
  "snippets",
  "workflows",
  "help",
  "search",
  "gateway",
  "today",
  "knowledge-graph",
  "dep-graph",
  "metrics",
  "bookmarks",
  "arena",
  "channels",
  "multibuffer",
  "lanes",
  "cookbook",
  "research",
  "routines",
  "eval",
  "setup",
];

export function App() {
  const setHasApiKey = useCortexStore((s) => s.setHasApiKey);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);
  const currentMode = useCortexStore((s) => s.currentMode);
  const setCurrentMode = useCortexStore((s) => s.setCurrentMode);
  const statusBarCompact = useCortexStore((s) => s.statusBarCompact);
  const setStatusBarCompact = useCortexStore((s) => s.setStatusBarCompact);
  const appendMessage = useCortexStore((s) => s.appendMessage);
  const setShowSessionPicker = useCortexStore((s) => s.setShowSessionPicker);
  const activityTab = useCortexStore((s) => s.activityTab);
  const archOpen = useArchTabOpen();
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const [rightTab, setRightTab] = useState<RightTab>("memory");
  const [showShortcuts, setShowShortcuts] = useState(false);

  // Re-apply the user's persisted theme on launch (saved to ~/.cortex via the
  // backend). Without this the theme picker's choice is lost on every restart.
  useThemeBoot();

  // Linux-native E2E probe. Inert unless launched with CORTEX_E2E=1, at which
  // point it heartbeats renderer state to ~/.cortex/e2e/snapshot.json so a
  // headless runner can verify the build actually painted (see e2e-probe.ts).
  useE2EProbe();

  // Auto-condense the conversation when its estimated context crosses the
  // configured threshold (Cline "Condense Context" on overflow). No-op unless
  // enabled in Settings → Advanced.
  useAutoCondense();

  // Restore the user's last layout (active panel + worktree selection) and keep
  // it persisted across the session. lib/ui-persistence.ts had never been wired
  // in, so these silently reset to defaults on every launch.
  useEffect(() => {
    const saved = loadUIState();
    if (saved) {
      if (saved.activityTab) {
        useCortexStore.getState().setActivityTab(saved.activityTab);
      }
      if (saved.currentWorktreeId) {
        useCortexStore
          .getState()
          .setCurrentWorktree(saved.currentWorktreeId, saved.currentWorktreePath);
      }
    }
    const detachUI = attachUIStatePersistence();
    const detachMirror = attachPrefMirror();
    return () => {
      detachUI();
      detachMirror();
    };
  }, []);

  // Expose a minimal driver hook for E2E audits (scripts/e2e-audit.mjs).
  // The hook isn't gated on dev mode because tauri-driver runs against the
  // production exe; production users will never call it from devtools.
  useEffect(() => {
    (window as unknown as { __cortexTabSwitch?: (t: ActivityTab) => void }).__cortexTabSwitch = (t) => {
      useCortexStore.getState().setActivityTab(t);
    };
    return () => {
      delete (window as unknown as { __cortexTabSwitch?: unknown }).__cortexTabSwitch;
    };
  }, []);

  useEffect(() => {
    getGatewayConfig()
      .then((cfg) => {
        setHasApiKey(cfg.has_api_key);
        if (!cfg.has_api_key) setShowSettings(true);
      })
      .catch(() => {});
  }, [setHasApiKey, setShowSettings]);

  // Surface `monitor-line` events from the Rust monitor runtime as synthetic
  // system messages in the active chat. Per-monitor rate-limit: if a single
  // monitor emits more than 10 lines/sec we drop the surplus on the floor
  // (backend already caps at 100/sec; this is the chat-readable cap).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;
    // monitor-name → { windowStart, count }
    const rate = new Map<string, { start: number; count: number }>();
    subscribeMonitorLines((p) => {
      if (!mounted) return;
      const now = Date.now();
      const slot = rate.get(p.name);
      if (!slot || now - slot.start >= 1000) {
        rate.set(p.name, { start: now, count: 1 });
      } else if (slot.count >= 10) {
        // Drop — too noisy. The backend already emitted a [rate-limited]
        // notice on its side, so we don't double-report here.
        return;
      } else {
        slot.count += 1;
      }
      const role = p.level === "error" ? "error" : "system";
      appendMessage({
        id: `mon-${crypto.randomUUID()}`,
        role,
        agent: `monitor:${p.name}`,
        content: `[monitor:${p.name}] ${p.line}`,
        tools: [],
      });
    }).then((u) => {
      // If the component unmounted before the subscribe promise resolved,
      // tear the listener down immediately so it doesn't leak for the
      // process lifetime (StrictMode mounts/unmounts effects twice).
      if (!mounted) {
        u();
        return;
      }
      unlisten = u;
    });
    return () => {
      mounted = false;
      unlisten?.();
    };
  }, [appendMessage]);

  useEffect(() => {
    const cycleCombo = DEFAULT_KEYMAP.find((b) => b.id === "cycle-mode")?.combo ?? "Ctrl+M";
    const onKey = (e: KeyboardEvent) => {
      // Ctrl+Shift+F → focus Memory search.
      if (matchCombo(e, "Ctrl+Shift+F")) {
        e.preventDefault();
        setRightTab("memory");
        // Defer until the panel is mounted, then focus the search input.
        setTimeout(() => {
          const el = document.querySelector<HTMLInputElement>(".memex-search input");
          el?.focus();
          el?.select();
        }, 30);
        return;
      }
      // Ctrl+? → toggle the keyboard-shortcuts cheat sheet. `?` is Shift+/ on
      // a US layout, so the firing combo is Ctrl+Shift+/. We also accept the
      // keymap's plain Ctrl+/ so either keystroke opens the sheet.
      if (matchCombo(e, "Ctrl+Shift+/") || matchCombo(e, "Ctrl+/")) {
        e.preventDefault();
        setShowShortcuts((v) => !v);
        return;
      }
      // Ctrl+R → open the session resume picker. preventDefault keeps the
      // browser/webview from hard-reloading the app. Skipped while typing in
      // an editable field so a literal "r" still reaches the composer.
      if (matchCombo(e, "Ctrl+R")) {
        const target = e.target as HTMLElement | null;
        const tag = target?.tagName?.toLowerCase();
        if (tag === "input" || tag === "textarea" || target?.isContentEditable) return;
        e.preventDefault();
        setShowSessionPicker(true);
        return;
      }
      // Ctrl+. → toggle StatusBar compact mode. Skip when the user is
      // typing in an editable field so a literal "." keeps going to the
      // input even while Ctrl is held by, say, a modifier-sticky a11y tool.
      if (matchCombo(e, "Ctrl+.")) {
        const target = e.target as HTMLElement | null;
        const tag = target?.tagName?.toLowerCase();
        if (tag === "input" || tag === "textarea" || target?.isContentEditable) return;
        e.preventDefault();
        const next = !statusBarCompact;
        setStatusBarCompact(next);
        pushToast({
          title: next ? "status bar: compact" : "status bar: full",
          kind: "info",
          ttlMs: 1500,
        });
        return;
      }
      if (!matchCombo(e, cycleCombo)) return;
      // Avoid stealing keystrokes while the user is typing in an input.
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      if (tag === "input" || tag === "textarea" || target?.isContentEditable) return;
      e.preventDefault();
      setCurrentMode(currentMode === "plan" ? "act" : "plan");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [currentMode, setCurrentMode, statusBarCompact, setStatusBarCompact, setShowSessionPicker]);

  // Ctrl+Tab / Ctrl+Shift+Tab → cycle through ActivityPanel tabs in the
  // declared order from `state/store.ts`. Skips `null` (the "no tab"
  // sentinel), wraps at both ends, and ignores keystrokes while the user
  // is typing in an input/textarea/contentEditable so the cycle never
  // steals tab-completion from the composer.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !e.ctrlKey || e.altKey || e.metaKey) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      if (tag === "input" || tag === "textarea" || target?.isContentEditable) return;
      e.preventDefault();
      const order = ACTIVITY_TAB_ORDER;
      if (order.length === 0) return;
      const current = activityTab;
      // When no tab is active (or the active tab isn't in the cycle order),
      // Ctrl+Tab lands on the first and Ctrl+Shift+Tab on the last — feels
      // more natural than a no-op for the empty state.
      const idx = current == null ? -1 : order.indexOf(current);
      // `order.indexOf` returns -1 for a tab that isn't part of the cycle,
      // which we deliberately fold into the empty-state behavior below.
      const len = order.length;
      const delta = e.shiftKey ? -1 : 1;
      const startIdx = idx < 0 ? (delta === 1 ? -1 : 0) : idx;
      const nextIdx = ((startIdx + delta) % len + len) % len;
      setActivityTab(order[nextIdx]);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activityTab, setActivityTab]);

  return (
    <div className="cortex-shell">
      <SurfaceLayer>
      <div className={`cortex-grid ${archOpen || (activityTab && activityTab !== "projects") ? "with-activity-panel" : ""}`}>
        <ActivityBar />
        <div className="cortex-col-left">
          <ProjectSidebar />
          <SidebarResizer />
        </div>
        {/* Always mounted: ActivityPanel self-hides when closed so its
            keep-alive surfaces (terminal PTY, unsaved editor buffers)
            survive closing the panel, not just switching tabs. */}
        <ActivityPanel />
        <div className="cortex-col-center">
          <TrustBanner />
          <ChatPane />
          <ObservabilityPanel />
        </div>
        <div className="cortex-col-right">
          <SidebarResizer side="right" />
          <div className="right-tabs">
            <button
              className={`right-tab ${rightTab === "memory" ? "active" : ""}`}
              onClick={() => setRightTab("memory")}
              title="Search memory, runbooks, and previous Claude chats (Ctrl+Shift+F)"
            >
              Memory
            </button>
            <button
              className={`right-tab ${rightTab === "brain" ? "active" : ""}`}
              onClick={() => setRightTab("brain")}
              title="Recent sessions, projects, memory snapshots"
            >
              Brain
            </button>
            <button
              className={`right-tab ${rightTab === "chats" ? "active" : ""}`}
              onClick={() => setRightTab("chats")}
              title="All Claude/Cortex Gateway chat sessions, grouped by project"
            >
              Chats
            </button>
            <button
              className={`right-tab ${rightTab === "agent" ? "active" : ""}`}
              onClick={() => setRightTab("agent")}
              title="Active agents and capabilities"
            >
              Agent
            </button>
          </div>
          <div className="right-tab-body">
            {/* Ambient side panel: don't steal focus from the chat composer on
                launch (and don't leave a permanent focus-ring glow in the chrome). */}
            {rightTab === "memory" && <MemoryExplorer autoFocus={false} />}
            {rightTab === "brain" && <BrainPanel />}
            {rightTab === "chats" && <ChatHistorySidebar />}
            {rightTab === "agent" && <AgentSidebar />}
          </div>
        </div>
      </div>
      </SurfaceLayer>
      <StatusBar />
      <SettingsModal />
      <ShortcutsModal open={showShortcuts} onClose={() => setShowShortcuts(false)} />
      <SessionPicker />
      <CommandPalette />
      <ToastRack />
      <DialogHost />
      <CheckpointReviewHost />
      <AutoUpdater />
      <OnboardingWizard />
      <OnboardingGate />
    </div>
  );
}

/**
 * The tour only renders AFTER the wizard completes. Otherwise both the
 * fullscreen wizard modal and the bottom-right tour card overlap on
 * first launch, blocking the actual UI and confusing the user.
 */
function OnboardingGate() {
  const done = useCortexStore((s) => s.onboardingComplete);
  return done ? <OnboardingTour /> : null;
}
