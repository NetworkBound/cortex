// Single source of truth for the Bookmarks panel's BookmarkKind → icon mapping.
//
// The bookmark kind chips + group headers used full-color emoji (🗂 memory,
// 📄 file, 🔭 trace, 💬 session, 🌐 url, 📝 note) — the same ransom-note tell
// the @-picker (vocab-icons.tsx) and nav (activity-icons.tsx) already fixed:
// saturated emoji next to bare-text chips, every glyph a different baseline.
// This maps every BookmarkKind onto the Lucide set the rest of the app uses.
//
// The exhaustive Record makes a missing kind a compile error.

import type { LucideIcon } from "lucide-react";
import {
  Database,
  FileText,
  Telescope,
  MessagesSquare,
  Globe,
  StickyNote,
} from "lucide-react";
import type { BookmarkKind } from "@/lib/bookmarks";

export const BOOKMARK_ICONS: Record<BookmarkKind, LucideIcon> = {
  memory: Database,
  file: FileText,
  trace: Telescope,
  session: MessagesSquare,
  url: Globe,
  note: StickyNote,
};

interface BookmarkIconProps {
  kind: BookmarkKind;
  size?: number;
  strokeWidth?: number;
  className?: string;
}

// Render the icon for a bookmark kind. Defaults match the app-wide icon
// convention (14px, 1.75 stroke, `currentColor` so it inherits the chip/row
// text color in every state — see vocab-icons.tsx / activity-icons.tsx).
export function BookmarkIcon({ kind, size = 14, strokeWidth = 1.75, className }: BookmarkIconProps) {
  const Icon = BOOKMARK_ICONS[kind];
  return <Icon size={size} strokeWidth={strokeWidth} className={className} aria-hidden="true" />;
}
