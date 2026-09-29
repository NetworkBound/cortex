import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useCortexStore } from "@/state/store";
import { ACTIVITY_TABS } from "@/lib/activity-tabs";
import { addProjectViaDialog } from "@/lib/add-project";
import { brainSnapshot, type RecentSession } from "@/lib/brain";
import { comboFor, formatCombo, matchCombo } from "@/lib/keymap";
import {
  cycleTheme,
  focusComposer,
  jumpToPendingApproval,
  loadRecents,
  newRoutine,
  pendingApprovalCount,
  rankPalette,
  recordRecent,
  revealSettingsSection,
  settingsTabLabel,
  type PaletteGroup,
  type PaletteItem,
} from "@/lib/palette-index";
import { applyProfile, listProfiles, type Profile } from "@/lib/profiles";
import {
  COMMANDS,
  CATEGORY_ORDER,
  categorize,
  makeContext,
  type SlashCommand,
} from "@/lib/slash-commands";
import { deriveThreadTitle } from "@/lib/threads";
import { timeAgo } from "@/lib/time";
import type { SettingsSectionMeta } from "./settings/sections";
import { openShortcutsModal } from "./ShortcutsModal";

/** Group header order for the browse view. Slash-command categories follow
 *  (their own "Go to" / "Project" / "Workflow" buckets merge with ours). */
const SECTION_ORDER: readonly string[] = [
  "Recent",
  "Actions",
  "Go to",
  "Threads",
  "Sessions",
  "Settings",
  "Project",
  "Workflow",
  ...CATEGORY_ORDER,
];

const PALETTE_COMBO = comboFor("palette", "Ctrl+K");

/** Shorten a first message into a one-line session title. */
function sessionLabel(s: RecentSession): string {
  const raw = s.first_message?.replace(/\s+/g, " ").trim();
  if (raw) return raw.length > 60 ? raw.slice(0, 60) + "…" : raw;
  return `Session ${s.session_id.slice(-8)}`;
}

export function CommandPalette() {
  const open = useCortexStore((s) => s.showCommandPalette);
  const setOpen = useCortexStore((s) => s.setShowCommandPalette);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const resetSession = useCortexStore((s) => s.resetSession);
  const projects = useCortexStore((s) => s.projects);
  const activeProject = useCortexStore((s) => s.activeProject);
  const setActive = useCortexStore((s) => s.setActiveProject);
  const setCurrentProfile = useCortexStore((s) => s.setCurrentProfile);
  const currentProfile = useCortexStore((s) => s.currentProfile);
  const currentMode = useCortexStore((s) => s.currentMode);
  const setCurrentMode = useCortexStore((s) => s.setCurrentMode);
  const threads = useCortexStore((s) => s.threads);
  const activeThreadId = useCortexStore((s) => s.activeThreadId);
  const switchThread = useCortexStore((s) => s.switchThread);
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [settingsIndex, setSettingsIndex] = useState<SettingsSectionMeta[]>([]);
  const [recents, setRecents] = useState(() => loadRecents());
  // Collapsed categories are tracked by name. Empty set ⇒ all expanded
  // (the spec's default state). Header click toggles membership.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const listRef = useRef<HTMLUListElement>(null);

  // Refresh the profile list every time the palette is opened against the
  // current project. Cheap (one fs read of `<root>/.cortex/profiles/`) and
  // avoids stale data after editing a TOML on disk.
  useEffect(() => {
    if (!open) return;
    const root = activeProject?.root;
    if (!root) {
      setProfiles([]);
      return;
    }
    let cancelled = false;
    listProfiles(root)
      .then((list) => {
        if (!cancelled) setProfiles(list);
      })
      .catch(() => {
        if (!cancelled) setProfiles([]);
      });
    return () => {
      cancelled = true;
    };
  }, [open, activeProject?.root]);

  // On open: reload recents, pull recent sessions from the brain snapshot,
  // and fetch the settings section index. The latter lives in the (lazy)
  // Settings chunk, so it is imported on demand rather than at boot.
  useEffect(() => {
    if (!open) return;
    setRecents(loadRecents());
    let cancelled = false;
    brainSnapshot()
      .then((snap) => {
        if (!cancelled) setSessions(snap.recent_sessions ?? []);
      })
      .catch(() => {
        if (!cancelled) setSessions([]);
      });
    void import("./settings/sections")
      .then((m) => {
        if (!cancelled) setSettingsIndex(m.SETTINGS_SECTION_INDEX);
      })
      .catch(() => {
        if (!cancelled) setSettingsIndex([]);
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  const close = useCallback(
    (dismissed: boolean) => {
      setOpen(false);
      setQ("");
      setIdx(0);
      // Esc / backdrop: hand focus back to the composer so the next keystroke
      // lands in the draft. Items that open another surface manage their own.
      if (dismissed) focusComposer();
    },
    [setOpen],
  );

  const items = useMemo<PaletteItem[]>(() => {
    const c: PaletteItem[] = [];
    const action = (
      id: string,
      label: string,
      run: () => void | Promise<void>,
      opts: { combo?: string; keywords?: string; hint?: string } = {},
    ) =>
      c.push({
        id,
        kind: "action",
        label,
        hint: opts.combo ? formatCombo(opts.combo) : opts.hint,
        keywords: opts.keywords,
        section: "Actions",
        run,
      });

    action(
      "new-chat",
      "New chat",
      () => {
        resetSession();
        focusComposer();
      },
      { combo: comboFor("new-session"), keywords: "session reset fresh" },
    );
    action("settings", "Open settings", () => setShowSettings(true), {
      combo: comboFor("settings"),
      keywords: "preferences config",
    });
    action(
      "add-project",
      "Open project folder…",
      () => void addProjectViaDialog({ title: "Open project folder" }),
      { keywords: "add repo repository directory import new project" },
    );
    action("new-routine", "New routine", newRoutine, {
      keywords: "schedule automation cron",
    });
    const pending = pendingApprovalCount();
    if (pending > 0) {
      action(
        "focus-approval",
        `Jump to pending approval (${pending})`,
        () => void jumpToPendingApproval(),
        { combo: comboFor("focus-approval"), keywords: "approve tool call" },
      );
    }
    action("toggle-theme", "Toggle theme", () => void cycleTheme(), {
      combo: comboFor("cycle-theme"),
      keywords: "dark light appearance colors",
    });
    action(
      "toggle-mode",
      `Toggle Plan / Act mode (now: ${currentMode})`,
      () => setCurrentMode(currentMode === "plan" ? "act" : "plan"),
      { combo: comboFor("cycle-mode"), keywords: "planning acting" },
    );
    action(
      "quick-open",
      "Quick open file…",
      () => void import("@/lib/quick-open").then((m) => m.openQuickOpen()),
      { combo: comboFor("quickopen"), keywords: "find file path recent" },
    );
    action("shortcuts", "Keyboard shortcuts", openShortcutsModal, {
      combo: comboFor("shortcuts"),
      keywords: "keys cheat sheet help",
    });

    // Every activity surface gets a "Go to …" entry so the full nav is
    // discoverable from Ctrl+K, not just the rail. Labels/order come from the
    // single tab registry (lib/activity-tabs), so this list can't drift.
    for (const t of ACTIVITY_TABS) {
      c.push({
        id: `tab-${t.id}`,
        kind: "nav",
        label: `Go to ${t.title}`,
        hint: t.group,
        keywords: t.label,
        section: "Go to",
        run: () => setActivityTab(t.id),
      });
    }

    // Threads (parallel chat lanes) — title + relative time, newest first.
    const sortedThreads = [...threads].sort((a, b) => b.lastTs - a.lastTs);
    for (const t of sortedThreads) {
      const isActive = t.id === activeThreadId;
      c.push({
        id: `thread-${t.id}`,
        kind: "thread",
        label: `${deriveThreadTitle(t)}${isActive ? " ✓" : ""}`,
        hint: timeAgo(t.lastTs, { coarse: true }),
        keywords: "thread chat",
        section: "Threads",
        run: () => {
          if (!isActive) switchThread(t.id);
          focusComposer();
        },
      });
    }

    // Persisted sessions not currently open as a thread; resume via the same
    // `cortex:chat-replay` event the Chats sidebar and Brain panel use.
    const openSessionIds = new Set(threads.map((t) => t.sessionId));
    for (const s of sessions.slice(0, 40)) {
      if (openSessionIds.has(s.session_id)) continue;
      c.push({
        id: `session-${s.session_id}`,
        kind: "session",
        label: sessionLabel(s),
        hint: timeAgo(s.last_active_ms, { coarse: true }),
        keywords: `session resume ${s.agents.join(" ")}`,
        section: "Sessions",
        run: () =>
          window.dispatchEvent(
            new CustomEvent("cortex:chat-replay", {
              detail: { session_id: s.session_id },
            }),
          ),
      });
    }

    // Settings sections → open the modal on that tab and scroll to the card.
    for (const s of settingsIndex) {
      c.push({
        id: `settings-${s.tab}-${s.heading}`,
        kind: "settings",
        label: `Settings › ${settingsTabLabel(s.tab)} › ${s.heading}`,
        keywords: s.text,
        section: "Settings",
        run: () => revealSettingsSection(s.tab, s.heading),
      });
    }

    for (const p of projects) {
      c.push({
        id: `pj-${p.root}`,
        kind: "project",
        label: `Switch project → ${p.name}`,
        hint: p.has_git ? "git" : "",
        keywords: p.root,
        section: "Project",
        run: () => setActive(p),
      });
    }
    const root = activeProject?.root;
    if (root) {
      for (const prof of profiles) {
        const isActive = currentProfile?.name === prof.name;
        c.push({
          id: `profile-${prof.name}`,
          kind: "profile",
          label: `Profile: ${prof.name}${isActive ? " ✓" : ""}`,
          hint: prof.sandbox_tier ?? prof.model ?? "",
          section: "Workflow",
          run: () =>
            void applyProfile(root, prof.name)
              .then((p) => setCurrentProfile(p))
              .catch((e) => console.error("apply_profile failed", e)),
        });
      }
    }
    // Slash commands — one palette entry per canonical name. Aliases are
    // intentionally collapsed (they all dispatch to the same `run`); they stay
    // searchable through `keywords` without diluting the list.
    for (const sc of COMMANDS as SlashCommand[]) {
      const cat = sc.category ?? categorize(sc.name);
      c.push({
        id: `slash-${sc.name}`,
        kind: "slash",
        label: `/${sc.name}${sc.usage ? ` ${sc.usage}` : ""} — ${sc.description}`,
        hint:
          sc.aliases && sc.aliases.length > 0
            ? sc.aliases.map((a) => `/${a}`).join(" ")
            : "",
        keywords: sc.aliases?.join(" "),
        section: cat,
        // Dispatch through the same SlashContext the chat input uses so
        // tab-switches, modal portals, and toasts all fire identically.
        run: () =>
          void Promise.resolve(sc.run("", makeContext())).catch((e) =>
            console.error(`/${sc.name} failed`, e),
          ),
      });
    }
    return c;
  }, [
    projects,
    activeProject,
    profiles,
    currentProfile,
    currentMode,
    threads,
    activeThreadId,
    sessions,
    settingsIndex,
    setShowSettings,
    setActivityTab,
    resetSession,
    setActive,
    setCurrentProfile,
    setCurrentMode,
    switchThread,
  ]);

  // Rank + group: fuzzy score with a recency boost, prefix modes, a "Recent"
  // group on the empty query. While searching every visible group is
  // forced-open so the user always sees the hits.
  const grouped = useMemo<PaletteGroup[]>(
    () =>
      rankPalette(items, q, {
        sectionOrder: SECTION_ORDER,
        recents,
        now: Date.now(),
      }),
    [items, q, recents],
  );

  const searching = q.trim().length > 0;

  // Flatten the visible (non-collapsed) rows in render order — drives
  // arrow-key navigation. When a search is active we ignore `collapsed`
  // so all matches stay reachable via keyboard.
  const visible = useMemo<PaletteItem[]>(() => {
    const out: PaletteItem[] = [];
    for (const g of grouped) {
      if (!searching && collapsed.has(g.section)) continue;
      out.push(...g.items);
    }
    return out;
  }, [grouped, collapsed, searching]);

  const runItem = useCallback(
    (item: PaletteItem) => {
      recordRecent(item.id);
      close(false);
      void Promise.resolve(item.run()).catch((e) =>
        console.error(`palette action ${item.id} failed`, e),
      );
    },
    [close],
  );

  // Reset highlight whenever the visible set changes shape (search, collapse).
  useEffect(() => {
    setIdx(0);
  }, [q, collapsed]);

  // Keep the keyboard-highlighted row visible as the user arrows past the
  // fold. `block: "nearest"` only scrolls when the row is actually offscreen,
  // so it doesn't jump on every keystroke.
  useEffect(() => {
    listRef.current
      ?.querySelector(".active")
      ?.scrollIntoView({ block: "nearest" });
  }, [idx]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (matchCombo(e, PALETTE_COMBO)) {
        e.preventDefault();
        if (open) close(true);
        else {
          setQ("");
          setIdx(0);
          setOpen(true);
        }
      } else if (e.key === "Escape" && open) {
        e.preventDefault();
        close(true);
      } else if (open && e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, visible.length - 1));
      } else if (open && e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      } else if (open && e.key === "Enter") {
        e.preventDefault();
        const item = visible[idx];
        if (item) runItem(item);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, setOpen, close, runItem, visible, idx]);

  if (!open) return null;

  const toggleCategory = (cat: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  };

  // Build a flat-index lookup so the per-group render can highlight the
  // single active row across the whole list.
  let runningIndex = 0;

  return (
    <div className="palette-backdrop" onClick={() => close(true)}>
      <div
        className="palette"
        role="dialog"
        aria-label="Command palette"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          autoFocus
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            setIdx(0);
          }}
          placeholder="Search commands, panels, threads, settings…"
          aria-label="Search commands"
        />
        <ul ref={listRef}>
          {grouped.length === 0 && <li className="muted">no matches</li>}
          {grouped.map((g) => {
            const isCollapsed = !searching && collapsed.has(g.section);
            return (
              <li key={`group-${g.section}`} className="palette-group">
                <button
                  type="button"
                  className="palette-category"
                  onClick={() => toggleCategory(g.section)}
                  aria-expanded={!isCollapsed}
                >
                  <span className="palette-category-caret">
                    {isCollapsed ? (
                      <ChevronRight size={14} strokeWidth={1.75} />
                    ) : (
                      <ChevronDown size={14} strokeWidth={1.75} />
                    )}
                  </span>
                  <span className="palette-category-name">{g.section}</span>
                  <span className="palette-category-count">
                    {g.items.length}
                  </span>
                </button>
                {!isCollapsed && (
                  <ul className="palette-category-items">
                    {g.items.map((c) => {
                      const i = runningIndex++;
                      return (
                        <li
                          key={c.id}
                          className={i === idx ? "active" : ""}
                          onMouseEnter={() => setIdx(i)}
                          onClick={() => runItem(c)}
                        >
                          <span>{c.label}</span>
                          {c.hint && (
                            <span className="palette-hint">{c.hint}</span>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
        <div className="palette-footer">
          <span>
            <kbd>&gt;</kbd> commands
          </span>
          <span>
            <kbd>@</kbd> threads
          </span>
          <span>
            <kbd>#</kbd> settings
          </span>
          <span>
            <kbd>/</kbd> slash
          </span>
          <span className="palette-footer-spacer" />
          <span>
            <kbd>↑↓</kbd> navigate <kbd>↵</kbd> run <kbd>Esc</kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}
