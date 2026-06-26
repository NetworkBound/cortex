/**
 * SurfaceLayer
 *
 * Translucent layer that sits between the optional user-supplied background
 * image and the actual app content. Two stacked fixed `<div>`s:
 *
 *   1. `.cortex-bg-image` — paints the image at ~8% opacity, full-bleed,
 *      `pointer-events: none`. Hidden entirely when there's no image.
 *   2. `.cortex-bg-overlay` — solid `--bg`-tinted overlay that keeps text
 *      legible regardless of the image colors. Always present so removing
 *      the overlay can never expose raw image pixels behind UI chrome.
 *
 * The actual app grid renders as `children` *on top* of both. We keep this
 * component dumb on purpose: it just renders a portal-ish frame; the active
 * theme is applied globally on first paint by `useThemeBoot` (wired into
 * `App.tsx`), and the image path is read here on mount.
 */
import { useEffect, useState } from "react";
import { bgImageAssetUrl } from "../lib/bg-image";
import { getActiveThemeState } from "../lib/themes-custom";

interface SurfaceLayerProps {
  /** Override the persisted bg image path. Mainly useful for previews. */
  bgImageUrl?: string | null;
  children: React.ReactNode;
}

export function SurfaceLayer({ bgImageUrl, children }: SurfaceLayerProps) {
  const [persistedUrl, setPersistedUrl] = useState<string | null>(null);

  // Read the persisted background path on mount. We only do this when the
  // caller hasn't passed an explicit override.
  useEffect(() => {
    if (bgImageUrl !== undefined) return;
    let cancelled = false;
    getActiveThemeState()
      .then((state) => {
        if (cancelled) return;
        setPersistedUrl(bgImageAssetUrl(state.bg_image_path));
      })
      .catch(() => {
        // No-op — leave persistedUrl as null so we render the plain overlay.
      });
    return () => {
      cancelled = true;
    };
  }, [bgImageUrl]);

  const effectiveUrl =
    bgImageUrl !== undefined ? bgImageUrl : persistedUrl;

  // Only render the dimming overlay when there's actually an image behind
  // it. Without that guard, the overlay (z-index: 1, opacity: 0.92) covers
  // the content because `.cortex-surface` uses `display: contents` and
  // therefore can't establish a stacking context — the text-on-app reads at
  // ~8% visibility, which is what bit user's 2026-05-27 build.
  return (
    <>
      {effectiveUrl ? (
        <>
          <div
            className="cortex-bg-image"
            aria-hidden="true"
            style={{ backgroundImage: `url("${effectiveUrl}")` }}
          />
          <div className="cortex-bg-overlay" aria-hidden="true" />
        </>
      ) : null}
      <div className="cortex-surface">{children}</div>
    </>
  );
}
