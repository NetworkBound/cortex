// Single source of truth for the @-mention picker's VocabKind → icon mapping.
//
// The @-picker (FilePicker.tsx, triggered by `@` in the chat composer) used a
// mix of full-color emoji for icons — some kinds carried an inline emoji in
// their label (🧠 Brain, 🌐 Web, 🔎 Grep, 📁 cwd…), most carried none, and the
// per-row glyph came from an ad-hoc `iconFor()` returning 📄/📁/🕘/📘/🧠/⚠…
// That reads as a ransom-note: saturated emoji next to bare-text rows, every
// glyph a different baseline and size. Best-in-class tools (Linear, Raycast,
// Zed, Cursor) give every picker row ONE cohesive monochrome line-icon that
// inherits the text color. This maps every VocabKind onto the Lucide set (the
// same set the rest of the app's chrome already uses, see activity-icons.tsx).
//
// The exhaustive Record makes a missing kind a compile error.

import type { LucideIcon } from "lucide-react";
import {
  BookText,
  Braces,
  Brain,
  CircleAlert,
  ClipboardList,
  Code,
  Database,
  FileClock,
  FileDiff,
  FileText,
  Folder,
  FolderTree,
  GitBranch,
  Globe,
  History,
  ListCollapse,
  ListTree,
  MessagesSquare,
  Puzzle,
  ScanSearch,
  Search,
  Telescope,
  SquareFunction,
  SquareTerminal,
  TriangleAlert,
  Variable,
  Waypoints,
} from "lucide-react";
import type { VocabKind } from "@/lib/at-vocab";

export const VOCAB_ICONS: Record<VocabKind, LucideIcon> = {
  files: FileText,
  folders: Folder,
  git: GitBranch,
  recent: History,
  docs: BookText,
  memory: Database,
  symbols: Braces,
  threads: MessagesSquare,
  diagnostics: TriangleAlert,
  snippets: Code,
  diff: FileDiff,
  problems: CircleAlert,
  terminal: SquareTerminal,
  brain: Brain,
  status: ClipboardList,
  "recent-edits": FileClock,
  frag: Puzzle,
  web: Globe,
  websearch: Telescope,
  grep: Search,
  codebase: ScanSearch,
  cwd: FolderTree,
  tree: ListTree,
  outline: ListCollapse,
  def: SquareFunction,
  refs: Waypoints,
  env: Variable,
};

interface VocabIconProps {
  kind: VocabKind;
  size?: number;
  strokeWidth?: number;
  className?: string;
}

// Render the icon for a vocab kind. Defaults: 14px, a 1.75 stroke (calmer than
// Lucide's default 2 at small sizes), `currentColor` so it inherits the row's
// text color in every state — matching the composer/nav icon conventions.
export function VocabIcon({ kind, size = 14, strokeWidth = 1.75, className }: VocabIconProps) {
  const Icon = VOCAB_ICONS[kind];
  return <Icon size={size} strokeWidth={strokeWidth} className={className} aria-hidden="true" />;
}
