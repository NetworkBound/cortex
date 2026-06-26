import { ChevronDown, ChevronRight } from "lucide-react";

/**
 * Canonical collapsible-section caret.
 *
 * Renders a Lucide chevron (down when open, right when collapsed) instead of a
 * unicode triangle glyph (▾/▸/▼/▶), so every expandable row in the app shares
 * one cohesive line-icon — the Linear/Raycast/Zed standard. Inherits the parent's
 * `currentColor` and centers via the caret span's own `inline-flex` layout.
 */
export function Chevron({
  open,
  size = 14,
}: {
  open: boolean;
  size?: number;
}) {
  const Icon = open ? ChevronDown : ChevronRight;
  return <Icon size={size} strokeWidth={1.75} aria-hidden="true" />;
}
