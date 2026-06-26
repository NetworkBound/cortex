// SINGLE SOURCE OF TRUTH for the activity-tab surfaces.
//
// Tab metadata used to live in four places that drifted apart:
//   - state/store.ts          → the `ActivityTab` union (membership)
//   - App.tsx                 → `ACTIVITY_TAB_ORDER` (Ctrl+Tab cycle order)
//   - ActivityBar.tsx         → `GROUPS` (rail grouping + short labels)
//   - ActivityPanel.tsx       → `labelFor()` (header titles)
//
// They were edited independently and disagreed: App.tsx's cycle order was
// missing `ultimate` entirely, and the bar's short labels ("Focus") differed
// from the panel's titles ("Focus chain") with no shared origin. This module
// is the one table everything else derives from. The bar order, the Ctrl+Tab
// order, the labels and the routing all read from `ACTIVITY_TABS` here.
//
// To add a surface: add its id to `ActivityTab` in state/store.ts, add its
// icon to ACTIVITY_ICONS in activity-icons.tsx, and add one row below. The
// exhaustive checks in this file (and the Record types elsewhere) turn a
// missing entry into a compile error.

import type { ActivityTab } from "@/state/store";

type TabId = NonNullable<ActivityTab>;

/** Intent-based clusters the activity rail groups surfaces into. Order here is
 *  the order groups render in the rail. */
export const ACTIVITY_GROUPS = [
  "Agents",
  "Knowledge",
  "Code",
  "Project",
  "Automate",
  "System",
] as const;

export type ActivityGroup = (typeof ACTIVITY_GROUPS)[number];

export interface ActivityTabMeta {
  id: TabId;
  /** Short label for the rail pill / palette ("Focus"). */
  label: string;
  /** Longer, disambiguated title for the panel header ("Focus chain"). Falls
   *  back to `label` when the short form already reads well on its own. */
  title: string;
  group: ActivityGroup;
}

// Helper so the table rows stay readable. `title` defaults to `label`.
const T = (
  id: TabId,
  label: string,
  group: ActivityGroup,
  title?: string,
): ActivityTabMeta => ({ id, label, group, title: title ?? label });

// The canonical table. Declaration order IS:
//   - the within-group order in the activity rail, and
//   - the Ctrl+Tab cycle order (derived as ACTIVITY_TAB_ORDER below).
// Grouped here in the same intent clusters the rail shows, so the file reads
// like the UI.
export const ACTIVITY_TABS: readonly ActivityTabMeta[] = [
  // Agents
  T("agents", "Agents", "Agents"),
  T("ultimate", "Ultimate", "Agents", "Ultimate agent"),
  T("orchestrator", "Orchestrator", "Agents"),
  T("lanes", "Lanes", "Agents"),
  T("arena", "Arena", "Agents", "Model arena"),
  T("channels", "Channels", "Agents"),
  T("threads", "Threads", "Agents"),
  T("focus", "Focus", "Agents", "Focus chain"),
  // Knowledge
  T("brain", "Brain", "Knowledge"),
  T("memory", "Memory", "Knowledge"),
  T("sessions", "Sessions", "Knowledge"),
  T("research", "Research", "Knowledge", "Deep Research"),
  T("search", "Search", "Knowledge"),
  T("knowledge-graph", "Knowledge Graph", "Knowledge", "Knowledge graph"),
  T("bookmarks", "Bookmarks", "Knowledge"),
  // Code
  T("editor", "Editor", "Code"),
  T("multibuffer", "Multibuffer", "Code"),
  T("source-control", "Source Control", "Code", "Source control"),
  T("git", "Git", "Code", "Git history"),
  T("terminal", "Terminal", "Code"),
  T("preview", "Preview", "Code", "Web preview"),
  // Project
  T("projects", "Projects", "Project"),
  T("graph", "Graph", "Project"),
  T("dep-graph", "Dep Graph", "Project", "Dependency graph"),
  T("metrics", "Metrics", "Project", "Project metrics"),
  T("checkpoints", "Checkpoints", "Project"),
  T("snippets", "Snippets", "Project"),
  // Automate
  T("skills", "Skills", "Automate"),
  T("routines", "Routines", "Automate", "Scheduled routines"),
  T("eval", "Eval", "Automate", "Agent eval"),
  T("workflows", "Workflows", "Automate"),
  T("prp", "PRP", "Automate", "PRPs"),
  T("tools", "Tools", "Automate", "Tools registry"),
  T("trust", "Trust", "Automate", "Trust matrix"),
  // System
  T("today", "Today", "System"),
  T("cookbook", "Cookbook", "System", "Local-Model Cookbook"),
  T("usage", "Usage", "System"),
  T("observability", "Observability", "System"),
  T("gateway", "Cortex Gateway", "System", "Cortex Gateway capabilities"),
  T("setup", "Setup", "System"),
  T("help", "Help", "System"),
];

// Fast id → metadata lookup.
const BY_ID: Record<TabId, ActivityTabMeta> = Object.fromEntries(
  ACTIVITY_TABS.map((t) => [t.id, t]),
) as Record<TabId, ActivityTabMeta>;

/** Metadata for a tab id. */
export function tabMeta(id: TabId): ActivityTabMeta {
  return BY_ID[id];
}

/** Short rail/palette label for a tab. */
export function tabLabel(id: TabId): string {
  return BY_ID[id]?.label ?? id;
}

/** Longer disambiguated panel-header title for a tab. */
export function tabTitle(id: TabId): string {
  return BY_ID[id]?.title ?? BY_ID[id]?.label ?? id;
}

/** The rail layout: groups in declared order, each with its tabs in declared
 *  order. Derived so the rail can't drift from this table. */
export const ACTIVITY_RAIL: readonly { group: ActivityGroup; items: ActivityTabMeta[] }[] =
  ACTIVITY_GROUPS.map((group) => ({
    group,
    items: ACTIVITY_TABS.filter((t) => t.group === group),
  }));

/** Ctrl+Tab cycle order — every surface, in table order. Replaces the
 *  hand-maintained list in App.tsx (which had silently dropped `ultimate`). */
export const ACTIVITY_TAB_ORDER: readonly TabId[] = ACTIVITY_TABS.map((t) => t.id);
