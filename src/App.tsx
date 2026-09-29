import { lazy, Suspense, useEffect, useState, type ReactNode } from "react";
import { ActivityBar } from "./components/ActivityBar";
import { AutoUpdater } from "./components/AutoUpdater";
import { ActivityPanel } from "./components/ActivityPanel";
import { TrustBanner } from "./components/TrustBanner";
import { ChatHistorySidebar } from "./components/ChatHistorySidebar";
import { ChatPane } from "./components/ChatPane";
import { CommandPalette } from "./components/CommandPalette";
import { OnboardingTour } from "./components/OnboardingTour";
import { ProjectSidebar } from "./components/ProjectSidebar";
import { SidebarResizer } from "./components/SidebarResizer";
import { ShortcutsModal } from "./components/ShortcutsModal";
import { SurfaceLayer } from "./components/SurfaceLayer";
import { PanelLoading } from "./components/Skeleton";
import { ToastRack } from "./components/ToastRack";
import { DialogHost } from "./components/DialogHost";
import { preloadMarkdownView } from "./components/MarkdownView";
import { useArchTabOpen } from "./lib/arch-tab";
import { useCheckpointReviewStore } from "./lib/checkpoint-review";
import { DEFAULT_KEYMAP, matchCombo } from "./lib/keymap";
import { isCtrlShiftCombo, isEditableTarget } from "./lib/keymap";
import { GLOBAL_KEYMAP_ACTIONS } from "./lib/palette-index";
import { subscribeMonitorLines } from "./lib/monitors";
import { runWhenIdle } from "./lib/scheduling";
import { useThemeBoot } from "./lib/use-theme-boot";
import { useE2EProbe } from "./lib/e2e-probe";
import { useAutoCondense } from "./lib/auto-condense";
import { attachUIStatePersistence, loadUIState } from "./lib/ui-persistence";
import { openProjectByPath } from "./lib/open-project";
import { attachPrefMirror } from "./lib/pref-sync";
import { useCortexStore } from "./state/store";
import { ACTIVITY_TAB_ORDER } from "./lib/activity-tabs";
import { startRepoWatcher, stopRepoWatcher } from "./lib/repo-watcher";
import { activateNotificationCenter } from "./lib/notification-center";
import { initJobStore } from "./state/jobs";
import { initRoutineNotifications } from "./lib/routines";

// Off-first-paint surfaces, code-split out of the startup bundle. Each is
// rendered through <MountOnceOpened>, which fetches the chunk the first time
// its open flag flips on and then keeps the component mounted for the rest of
// the session — exactly the lifetime it had when imported statically (all of
// them render `null` while closed and keep their local state across
// open/close), minus the parse cost at boot.
const SettingsModal = lazy(() =>
  import("./components/SettingsModal").then((m) => ({
    default: m.SettingsModal,
  })),
);
const SessionPicker = lazy(() =>
  import("./components/SessionPicker").then((m) => ({
    default: m.SessionPicker,
  })),
);
const CheckpointReviewHost = lazy(() =>
  import("./components/CheckpointReviewHost").then((m) => ({
    default: m.CheckpointReviewHost,
  })),
);
const OnboardingWizard = lazy(() =>
  import("./components/OnboardingWizard").then((m) => ({
    default: m.OnboardingWizard,
  })),
);
const AgentSidebar = lazy(() =>
  import("./components/AgentSidebar").then((m) => ({
    default: m.AgentSidebar,
  })),
);

/**
 * Renders nothing until `open` is first true, then mounts `children` inside a
 * Suspense boundary and never unmounts them again. Lazy modals get their chunk
 * on first open while keeping the "always mounted, renders null when closed"
 * behaviour they were written for (closed-state effects, retained local state).
 */
function MountOnceOpened({
  open,
  fallback = null,
  children,
}: {
  open: boolean;
  fallback?: ReactNode;
  children: ReactNode;
}) {
  const [visited, setVisited] = useState(open);
  if (open && !visited) setVisited(true);
  if (!open && !visited) return null;
  return <Suspense fallback={fallback}>{children}</Suspense>;
}

type RightTab = "agent" | "chats";

export function App() {
  const setHasApiKey = useCortexStore((s) => s.setHasApiKey);
  const currentMode = useCortexStore((s) => s.currentMode);
  const setCurrentMode = useCortexStore((s) => s.setCurrentMode);
  const appendMessage = useCortexStore((s) => s.appendMessage);
  const setShowSessionPicker = useCortexStore((s) => s.setShowSessionPicker);
  const activityTab = useCortexStore((s) => s.activityTab);
  const activeProject = useCortexStore((s) => s.activeProject);
  const archOpen = useArchTabOpen();
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const showSettings = useCortexStore((s) => s.showSettings);
  const showSessionPickerFlag = useCortexStore((s) => s.showSessionPicker);
  const onboardingComplete = useCortexStore((s) => s.onboardingComplete);
  const checkpointReviewOpen = useCheckpointReviewStore((s) => !!s.active);
  const [rightTab, setRightTab] = useState<RightTab>("chats");
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
          .setCurrentWorktree(
            saved.currentWorktreeId,
            saved.currentWorktreePath,
          );
      }
      if (saved.activeProjectRoot) {
        void openProjectByPath(saved.activeProjectRoot);
      }
    }
    const detachUI = attachUIStatePersistence();
    const detachMirror = attachPrefMirror();
    return () => {
      detachUI();
      detachMirror();
    };
  }, []);

  useEffect(() => {
    setHasApiKey(true);
  }, [setHasApiKey]);

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
    const cycleCombo =
      DEFAULT_KEYMAP.find((b) => b.id === "cycle-mode")?.combo ?? "Ctrl+M";
    const onKey = (e: KeyboardEvent) => {
      // Ctrl+Shift+F → open the Memory surface (activity panel) and focus its
      // search. Memory now has a single home in the activity bar (it used to be
      // duplicated as a right-rail tab too), so we route through the store.
      if (matchCombo(e, "Ctrl+Shift+F")) {
        e.preventDefault();
        setActivityTab("memory");
        // Defer until the panel is mounted, then focus the search input.
        setTimeout(() => {
          const el = document.querySelector<HTMLInputElement>(
            ".memex-search input",
          );
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
        if (tag === "input" || tag === "textarea" || target?.isContentEditable)
          return;
        e.preventDefault();
        setShowSessionPicker(true);
        return;
      }
      if (!matchCombo(e, cycleCombo)) return;
      // Avoid stealing keystrokes while the user is typing in an input.
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      if (tag === "input" || tag === "textarea" || target?.isContentEditable)
        return;
      e.preventDefault();
      setCurrentMode(currentMode === "plan" ? "act" : "plan");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [currentMode, setCurrentMode, setShowSessionPicker, setActivityTab]);

  // Global bindings for the DEFAULT_KEYMAP ids no component owns itself
  // (Ctrl+N new chat, Ctrl+P quick open, Ctrl+T cycle theme, Ctrl+Shift+C
  // compact, Ctrl+Shift+N new window, Ctrl+Shift+A jump to approval). The
  // palette, Settings and the cycle-mode / shortcuts / resume handlers above
  // bind their own ids, so GLOBAL_KEYMAP_ACTIONS (lib/palette-index) only
  // lists the rest — one table, no double binding. Single-modifier combos are
  // skipped while typing in an editable field; Ctrl+Shift chords fire anywhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      for (const b of DEFAULT_KEYMAP) {
        const action = GLOBAL_KEYMAP_ACTIONS[b.id];
        if (!action || !matchCombo(e, b.combo)) continue;
        if (isEditableTarget(e) && !isCtrlShiftCombo(b.combo)) return;
        e.preventDefault();
        action();
        return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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
      if (tag === "input" || tag === "textarea" || target?.isContentEditable)
        return;
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
      const nextIdx = (((startIdx + delta) % len) + len) % len;
      setActivityTab(order[nextIdx]);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activityTab, setActivityTab]);

  // ── Headless global side-effects (formerly StatusBar) ─────────────
  useEffect(() => {
    if (!activeProject) return;
    const root = activeProject.root;
    void startRepoWatcher(root).catch(() => {});
    return () => {
      void stopRepoWatcher(root).catch(() => {});
    };
  }, [activeProject]);
  // Non-critical boot work — notification streams + inbox pulls, in-flight
  // job adoption, routine toasts — runs once the browser is idle after the
  // first frames have painted. Nothing here is visible on the initial screen,
  // and each pull is a backend round trip we'd otherwise pay before paint.
  useEffect(() => {
    let deactivate: (() => void) | undefined;
    const cancel = runWhenIdle(() => {
      deactivate = activateNotificationCenter();
      initJobStore();
      initRoutineNotifications();
    }, 1_500);
    return () => {
      cancel();
      deactivate?.();
    };
  }, []);
  // Warm the markdown renderer chunk (react-markdown + highlight.js) right
  // after first paint so it's resident before the first message renders.
  useEffect(() => runWhenIdle(() => void preloadMarkdownView(), 300), []);

  return (
    <div className="cortex-shell">
      <SurfaceLayer>
        <div
          className={`cortex-grid ${archOpen || (activityTab && activityTab !== "projects") ? "with-activity-panel" : ""}`}
        >
          <ActivityBar />
          <div className="cortex-col-left">
            <ProjectSidebar />
            <SidebarResizer />
          </div>
          <ActivityPanel />
          <div className="cortex-col-center">
            <TrustBanner />
            <ChatPane />
          </div>
          <div className="cortex-col-right">
            <SidebarResizer side="right" />
            <div className="right-tabs">
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
              {rightTab === "chats" && <ChatHistorySidebar />}
              {rightTab === "agent" && (
                <Suspense fallback={<PanelLoading />}>
                  <AgentSidebar />
                </Suspense>
              )}
            </div>
          </div>
        </div>
      </SurfaceLayer>
      <MountOnceOpened open={showSettings}>
        <SettingsModal />
      </MountOnceOpened>
      <ShortcutsModal
        open={showShortcuts}
        onClose={() => setShowShortcuts(false)}
      />
      <MountOnceOpened open={showSessionPickerFlag}>
        <SessionPicker />
      </MountOnceOpened>
      <CommandPalette />
      <ToastRack />
      <DialogHost />
      <MountOnceOpened open={checkpointReviewOpen}>
        <CheckpointReviewHost />
      </MountOnceOpened>
      <AutoUpdater />
      {/* First-run only: the wizard's backdrop stands in while its chunk loads
          so a fresh install never sees the bare shell flash underneath. */}
      <MountOnceOpened
        open={!onboardingComplete}
        fallback={
          <div className="modal-backdrop onboarding-wizard" aria-busy="true" />
        }
      >
        <OnboardingWizard />
      </MountOnceOpened>
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
