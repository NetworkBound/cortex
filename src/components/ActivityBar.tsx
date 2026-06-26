import { useCallback, useEffect, useState } from "react";
import { useCortexStore, type ActivityTab } from "@/state/store";
import { ActivityIcon, ARCHITECTURE_ICON, SETTINGS_ICON } from "@/lib/activity-icons";
import { ACTIVITY_RAIL, type ActivityTabMeta } from "@/lib/activity-tabs";
import { Chevron } from "@/lib/chevron";
import { archTab, useArchTabOpen } from "./ArchitectureView";
import { SidebarResizer } from "./SidebarResizer";
import "../styles/activity-bar.css";

const COLLAPSE_KEY = "cortex.activityCollapsed";
const WIDTH_KEY = "cortex.activityWidth";
const COLLAPSED_W = 56;
const DEFAULT_W = 172;

const GROUPS_KEY = "cortex.navGroupsCollapsed";

// The activity surfaces, organized into intent-based clusters. The grouping,
// order, and labels all come from the single registry (lib/activity-tabs.ts)
// so the rail can't drift from the panel/palette/Ctrl+Tab cycle. Each group is
// independently collapsible (state persisted) and the group holding the active
// tab always renders open, so nothing gets lost.
const GROUPS = ACTIVITY_RAIL.map((g) => ({ label: g.group, items: g.items }));

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

  function renderItem(it: ActivityTabMeta) {
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
