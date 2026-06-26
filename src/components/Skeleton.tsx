import type { CSSProperties } from "react";

type SkeletonVariant = "text" | "block" | "circle";

/**
 * One shimmering placeholder block. `width`/`height` pass straight through to
 * inline styles so callers can shape it to the content it stands in for; the
 * `variant` only picks a corner radius (text → sm, block → md, circle → pill).
 * Purely decorative, so it's marked aria-hidden — the surrounding container
 * (see {@link SkeletonText}) is what announces the busy state.
 */
export function Skeleton({
  width,
  height,
  variant = "block",
  radius,
  className,
  style,
}: {
  width?: number | string;
  height?: number | string;
  variant?: SkeletonVariant;
  radius?: number | string;
  className?: string;
  style?: CSSProperties;
}) {
  return (
    <div
      className={`skeleton skeleton--${variant}${className ? ` ${className}` : ""}`}
      aria-hidden="true"
      style={{ width, height, borderRadius: radius, ...style }}
    />
  );
}

/**
 * A labeled stack of placeholder lines — the standard drop-in for a bare
 * "Loading…" string while real data loads into a known layout. The final line
 * is shortened so the block reads as text rather than a solid bar. The wrapper
 * carries role="status"/aria-busy so assistive tech announces the load.
 */
export function SkeletonText({
  lines = 3,
  lastLineWidth = "55%",
  label = "Loading",
  className,
}: {
  lines?: number;
  lastLineWidth?: number | string;
  label?: string;
  className?: string;
}) {
  return (
    <div
      className={`skeleton-stack${className ? ` ${className}` : ""}`}
      role="status"
      aria-busy="true"
      aria-label={label}
    >
      {Array.from({ length: lines }).map((_, i) => (
        <Skeleton
          key={i}
          variant="text"
          width={i === lines - 1 ? lastLineWidth : "100%"}
        />
      ))}
    </div>
  );
}

/**
 * Full-panel loading placeholder — a calm shimmer-skeleton stack wrapped in the
 * same generous padding as the unified empty-state contract, so a whole panel
 * that is still fetching reads as "content is arriving here" rather than a bare
 * top-left "loading…" string. This is the Linear/Vercel pattern and the
 * professional, consistent replacement for the ad-hoc muted "loading…" divs
 * panels used to each roll on their own (mismatched paddings + casing).
 */
export function PanelLoading({
  lines = 4,
  label = "Loading",
  className,
}: {
  lines?: number;
  label?: string;
  className?: string;
}) {
  return (
    <div className={`panel-loading${className ? ` ${className}` : ""}`}>
      <SkeletonText lines={lines} label={label} />
    </div>
  );
}
