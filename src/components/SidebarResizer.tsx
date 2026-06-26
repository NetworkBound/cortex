import { useCallback, useEffect, useRef } from "react";

/**
 * Drag handle that resizes one of the two side columns. Writes the chosen
 * width to a CSS variable on :root (the `.cortex-grid` template columns read
 * it) and persists it to localStorage so it survives reloads.
 *
 * `side="left"`  → project sidebar, var `--sidebar-w` (starts after the 56px
 *                  activity bar, so width = pointerX - 56).
 * `side="right"` → memory/brain/chats panel, var `--right-w` (anchored to the
 *                  window's right edge, so width = innerWidth - pointerX).
 *
 * Pure presentation + pointer math; nothing here touches app state or the
 * send pipeline. Double-click resets to the default width.
 */

type Side = "left" | "right" | "activity";

interface SpecShape {
  key: string;
  cssVar: string;
  def: number;
  min: number;
  max: number;
  widthFrom: (clientX: number) => number;
}

// The activity rail is itself resizable, so the project sidebar's width can no
// longer assume a fixed 56px rail — measure the rail's live width instead.
function activityRailWidth(): number {
  const el = document.querySelector(".activity-bar");
  if (el) return el.getBoundingClientRect().width;
  const v = parseInt(
    getComputedStyle(document.documentElement).getPropertyValue("--activity-w"),
    10,
  );
  return Number.isNaN(v) ? 172 : v;
}

const SPEC: Record<Side, SpecShape> = {
  // Far-left navigation rail (icon + label list).
  activity: {
    key: "cortex.activityWidth",
    cssVar: "--activity-w",
    def: 172,
    min: 132,
    max: 300,
    widthFrom: (clientX) => clientX,
  },
  left: {
    key: "cortex.sidebarWidth",
    cssVar: "--sidebar-w",
    def: 256,
    min: 200,
    max: 480,
    widthFrom: (clientX) => clientX - activityRailWidth(),
  },
  right: {
    key: "cortex.rightWidth",
    cssVar: "--right-w",
    def: 340,
    min: 260,
    max: 560,
    widthFrom: (clientX) => window.innerWidth - clientX,
  },
};

export function SidebarResizer({ side = "left" }: { side?: Side }) {
  const dragging = useRef(false);
  const spec = SPEC[side];

  const clamp = useCallback(
    (px: number) => Math.max(spec.min, Math.min(spec.max, Math.round(px))),
    [spec.min, spec.max],
  );

  const applyWidth = useCallback(
    (px: number) => document.documentElement.style.setProperty(spec.cssVar, `${px}px`),
    [spec.cssVar],
  );

  // Restore a persisted width on mount.
  useEffect(() => {
    try {
      const raw = localStorage.getItem(spec.key);
      if (raw) {
        const px = clamp(parseInt(raw, 10));
        if (!Number.isNaN(px)) applyWidth(px);
      }
    } catch {
      /* storage unavailable — fall back to the CSS default */
    }
  }, [spec.key, clamp, applyWidth]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.preventDefault();
      dragging.current = true;
      (e.target as HTMLElement).setPointerCapture(e.pointerId);
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";

      const onMove = (ev: PointerEvent) => {
        if (!dragging.current) return;
        applyWidth(clamp(spec.widthFrom(ev.clientX)));
      };
      const onUp = () => {
        dragging.current = false;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        const cur = getComputedStyle(document.documentElement)
          .getPropertyValue(spec.cssVar)
          .trim();
        try {
          if (cur) localStorage.setItem(spec.key, String(parseInt(cur, 10)));
        } catch {
          /* best-effort persistence */
        }
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [spec, clamp, applyWidth],
  );

  const onDoubleClick = useCallback(() => {
    applyWidth(spec.def);
    try {
      localStorage.setItem(spec.key, String(spec.def));
    } catch {
      /* best-effort */
    }
  }, [spec.def, spec.key, applyWidth]);

  return (
    <div
      className={`sidebar-resizer sidebar-resizer-${side}`}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize panel (double-click to reset)"
      title="Drag to resize · double-click to reset"
      onPointerDown={onPointerDown}
      onDoubleClick={onDoubleClick}
    />
  );
}
