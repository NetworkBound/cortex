import { useCallback, useEffect, useState } from "react";
import { useCortexStore, type ActivityTab } from "@/state/store";
import { ActivityIcon, ARCHITECTURE_ICON, SETTINGS_ICON } from "@/lib/activity-icons";
import { Chevron } from "@/lib/chevron";
import { archTab, useArchTabOpen } from "./ArchitectureView";
import { SidebarResizer } from "./SidebarResizer";
import "../styles/activity-bar.css";

const COLLAPSE_KEY = "cortex.activityCollapsed";
const WIDTH_KEY = "cortex.activityWidth";
const COLLAPSED_W = 56;
const DEFAULT_W = 172;

interface Item {
  id: NonNullable<ActivityTab>;
  label: string;
}

interface Group {
  label: string;
  items: Item[];
}

const GROUPS_KEY = "cortex.navGroupsCollapsed";

// Shorthand so the group tables below stay readable. The icon is resolved from
// the tab id at render time via <ActivityIcon>.
const I = (id: NonNullable<ActivityTab>, label: string): Item => ({ id, label });

// The 34 activity surfaces, organized into intent-based clusters instead of a
// flat Workspace/More split (which had ~25 items dumped under "More"). Each
// group is independently collapsible (state persisted) and the group holding
// the active tab always renders open, so nothing gets lost. Order within a
// group is most-used first.
const GROUPS: Group[] = [
  {
    label: "Agents",
    items: [
      I("agents", "Agents"),
      I("orchestrator", "Orchestrator"),
      I("lanes", "Lanes"),
      I("arena", "Arena"),
      I("channels", "Channels"),
      I("threads", "Threads"),
      I("focus", "Focus"),
    ],
  },
  {
    label: "Knowledge",
    items: [
      I("brain", "Brain"),
      I("memory", "Memory"),
      I("sessions", "Sessions"),
      I("research", "Research"),
      I("search", "Search"),
      I("knowledge-graph", "Knowledge Graph"),
      I("bookmarks", "Bookmarks"),
    ],
  },
  {
    label: "Code",
    items: [
      I("editor", "Editor"),
      I("multibuffer", "Multibuffer"),
      I("source-control", "Source Control"),
      I("git", "Git"),
      I("terminal", "Terminal"),
      I("preview", "Preview"),
    ],
  },
  {
    label: "Project",
    items: [
      I("projects", "Projects"),
      I("graph", "Graph"),
      I("dep-graph", "Dep Graph"),
      I("metrics", "Metrics"),
      I("checkpoints", "Checkpoints"),
      I("snippets", "Snippets"),
    ],
  },
  {
    label: "Automate",
    items: [
      I("skills", "Skills"),
      I("routines", "Routines"),
      I("eval", "Eval"),
      I("workflows", "Workflows"),
      I("prp", "PRP"),
      I("tools", "Tools"),
      I("trust", "Trust"),
    ],
  },
  {
    label: "System",
    items: [
      I("today", "Today"),
      I("cookbook", "Cookbook"),
      I("usage", "Usage"),
      I("observability", "Observability"),
      I("gateway", "Cortex Gateway"),
      I("setup", "Setup"),
      I("help", "Help"),
    ],
  },
];

export function ActivityBar() {
  const active = useCortexStore((s) => s.activityTab);
  const setActive = useCortexStore((s) => s.setActivityTab);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);
  const archOpen = useArchTabOpen();

  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(COLLAPSE_KEY) === "true";
    } catch {
      return false;
    }
  });

  // Per-group collapsed state (keyed by group label). Persisted so a tidied
  // rail stays tidy across launches.
  const [collapsedGroups, setCollapsedGroups] = useState<Record<string, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem(GROUPS_KEY) || "{}") as Record<string, boolean>;
    } catch {
      return {};
    }
  });

  const toggleGroup = useCallback((label: string) => {
    setCollapsedGroups((prev) => {
      const next = { ...prev, [label]: !prev[label] };
      try {
        localStorage.setItem(GROUPS_KEY, JSON.stringify(next));
      } catch {
        /* best-effort */
      }
      return next;
    });
  }, []);

  // Drive the grid column width off the collapsed state. When expanded we
  // restore the user's last dragged width (SidebarResizer persists it); when
  // collapsed we pin to a narrow icon-only strip.
  useEffect(() => {
    const root = document.documentElement;
    if (collapsed) {
      root.style.setProperty("--activity-w", `${COLLAPSED_W}px`);
    } else {
      let w = DEFAULT_W;
      try {
        const raw = localStorage.getItem(WIDTH_KEY);
        if (raw) {
          const px = parseInt(raw, 10);
          if (!Number.isNaN(px)) w = px;
        }
      } catch {
        /* fall back to default */
      }
      root.style.setProperty("--activity-w", `${w}px`);
    }
  }, [collapsed]);

  const toggleCollapsed = useCallback(() => {
    setCollapsed((c) => {
      const next = !c;
      try {
        localStorage.setItem(COLLAPSE_KEY, String(next));
      } catch {
        /* best-effort */
      }
      return next;
    });
  }, []);

  function pick(id: ActivityTab) {
    // Selecting a built-in tab dismisses the architecture panel.
    archTab.close();
    if (active === id) setActive(null);
    else setActive(id);
  }

  function renderItem(it: Item) {
    return (
      <button
        key={it.id}
        className={`activity-icon activity-tab-pill ${active === it.id ? "active" : ""}`}
        onClick={() => pick(it.id)}
        title={it.label}
        aria-label={it.label}
      >
        <span className="activity-glyph icon" aria-hidden="true">
          <ActivityIcon tab={it.id} />
        </span>
        <span className="activity-label label">{it.label}</span>
      </button>
    );
  }

  return (
    <nav className={`activity-bar ${collapsed ? "collapsed" : ""}`}>
      <div className="activity-bar-head">
        <span className="activity-bar-title">Cortex</span>
        <button
          className="activity-collapse-btn"
          onClick={toggleCollapsed}
          title={collapsed ? "Expand navigation" : "Collapse to icons"}
          aria-label={collapsed ? "Expand navigation" : "Collapse navigation"}
          aria-expanded={!collapsed}
        >
          {collapsed ? "»" : "«"}
        </button>
      </div>
      {collapsed
        ? // Icon-only rail: show every surface, just separated by group.
          GROUPS.map((g, i) => (
            <div className="activity-group" key={g.label}>
              {i > 0 && (
                <div className="activity-group-divider" role="separator" aria-label={g.label} />
              )}
              {g.items.map(renderItem)}
            </div>
          ))
        : // Expanded rail: collapsible group headers. A group always renders
          // open when it holds the active tab so the highlight is never hidden.
          GROUPS.map((g) => {
            const hasActive = g.items.some((it) => it.id === active);
            const open = !collapsedGroups[g.label] || hasActive;
            return (
              <div className="activity-group" key={g.label}>
                <button
                  className="activity-group-label activity-group-toggle"
                  onClick={() => toggleGroup(g.label)}
                  aria-expanded={open}
                  title={open ? `Collapse ${g.label}` : `Expand ${g.label}`}
                >
                  <span className="group-chevron" aria-hidden="true"><Chevron open={open} size={12} /></span>
                  {g.label}
                </button>
                {open && g.items.map(renderItem)}
              </div>
            );
          })}
      <button
        key="architecture"
        className={`activity-icon activity-tab-pill ${archOpen ? "active" : ""}`}
        onClick={() => archTab.toggle()}
        title="Architecture"
        aria-label="Architecture"
      >
        <span className="activity-glyph icon" aria-hidden="true">
          <ARCHITECTURE_ICON size={16} strokeWidth={1.75} />
        </span>
        <span className="activity-label label">Architecture</span>
      </button>
      <div className="activity-spacer" />
      <button
        className="activity-icon activity-tab-pill"
        onClick={() => setShowSettings(true)}
        title="Settings"
        aria-label="Settings"
      >
        <span className="activity-glyph icon" aria-hidden="true">
          <SETTINGS_ICON size={16} strokeWidth={1.75} />
        </span>
        <span className="activity-label label">Settings</span>
      </button>
      <SidebarResizer side="activity" />
    </nav>
  );
}
