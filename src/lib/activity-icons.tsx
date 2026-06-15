// Single source of truth for ActivityTab → icon mapping.
//
// Cortex's primary navigation (ActivityBar) and the right-panel header
// (ActivityPanel) used full-color emoji glyphs as icons — 🧠/🏆/🎯/💬/🔍/⭐/
// 📝/🌿/🌐 render in the system color-emoji font, so the nav was a jarring mix
// of saturated emoji next to a handful of monochrome ones. That's the single
// clearest "not professional" tell in the always-visible chrome. Best-in-class
// desktop/AI tools (Linear, Raycast, Zed, Cursor) use one cohesive, monochrome
// line-icon set that inherits the text color. This module maps every surface
// onto the Lucide line-icon set (the de-facto standard, used by those tools'
// kin) so the rail reads as a single coherent system.
//
// Keep keys in sync with ActivityTab in src/state/store.ts — the exhaustive
// Record type makes a missing tab a compile error.

import type { LucideIcon } from "lucide-react";
import {
  BarChart3,
  Bookmark,
  Bot,
  Braces,
  Brain,
  CalendarClock,
  ChefHat,
  Circle,
  CircleHelp,
  Database,
  Flag,
  Folder,
  Gauge,
  GitBranch,
  GitFork,
  GitPullRequest,
  Globe,
  Hash,
  Kanban,
  LayoutDashboard,
  LayoutGrid,
  LineChart,
  ListChecks,
  Map,
  MessageSquare,
  Microscope,
  MessagesSquare,
  Network,
  PlugZap,
  ScrollText,
  Search,
  Settings,
  Share2,
  ShieldCheck,
  Sparkles,
  SquarePen,
  SquareTerminal,
  Target,
  Telescope,
  Trophy,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";
import type { ActivityTab } from "@/state/store";

export const ACTIVITY_ICONS: Record<NonNullable<ActivityTab>, LucideIcon> = {
  brain: Brain,
  memory: Database,
  sessions: MessageSquare,
  projects: Folder,
  graph: Share2,
  agents: Bot,
  usage: BarChart3,
  observability: Telescope,
  checkpoints: Flag,
  threads: MessagesSquare,
  focus: Target,
  trust: ShieldCheck,
  skills: Sparkles,
  prp: ScrollText,
  preview: Globe,
  editor: SquarePen,
  terminal: SquareTerminal,
  git: GitBranch,
  "source-control": GitPullRequest,
  orchestrator: Workflow,
  tools: Wrench,
  snippets: Braces,
  search: Search,
  help: CircleHelp,
  gateway: Zap,
  workflows: ListChecks,
  today: LayoutDashboard,
  "knowledge-graph": Network,
  "dep-graph": GitFork,
  metrics: LineChart,
  bookmarks: Bookmark,
  arena: Trophy,
  channels: Hash,
  multibuffer: LayoutGrid,
  lanes: Kanban,
  cookbook: ChefHat,
  research: Microscope,
  routines: CalendarClock,
  eval: Gauge,
  setup: PlugZap,
};

// Non-tab surfaces that still live in the ActivityBar (architecture overlay,
// settings) get their own icons so the whole rail is one consistent set.
export const ARCHITECTURE_ICON: LucideIcon = Map;
export const SETTINGS_ICON: LucideIcon = Settings;

export function iconForTab(tab: NonNullable<ActivityTab>): LucideIcon {
  return ACTIVITY_ICONS[tab] ?? Circle;
}

interface ActivityIconProps {
  tab: NonNullable<ActivityTab>;
  size?: number;
  strokeWidth?: number;
  className?: string;
}

// Render the icon for a tab. Defaults match the rail glyph box: 16px, a 1.75
// stroke (a touch lighter than Lucide's 2 default, calmer at small sizes), and
// `currentColor` so it inherits the pill's text color in every state.
export function ActivityIcon({ tab, size = 16, strokeWidth = 1.75, className }: ActivityIconProps) {
  const Icon = iconForTab(tab);
  return <Icon size={size} strokeWidth={strokeWidth} className={className} aria-hidden="true" />;
}
