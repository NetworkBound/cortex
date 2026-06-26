import { useEffect } from "react";
import {
  applyCustomTheme,
  getActiveThemeState,
  resolveTheme,
} from "./themes-custom";

/**
 * Apply the persisted active theme once, on app mount.
 *
 * Picking a theme in Settings applies it live and saves the active name to
 * `~/.cortex/themes.json` via the Rust backend — but nothing re-applied it on
 * the next launch, so every restart silently reverted to the `global.css`
 * default. (See the historical note in `SurfaceLayer.tsx`: the active theme was
 * meant to be "set globally on first paint by `useThemeBoot` once wired in" —
 * this is that hook, finally wired in.)
 *
 * Best-effort: if the backend is unavailable during early boot, or no theme is
 * active yet (fresh install), we leave the `global.css` defaults in place.
 */
export function useThemeBoot(): void {
  useEffect(() => {
    let cancelled = false;
    getActiveThemeState()
      .then((state) => {
        if (cancelled || !state.active) return undefined;
        return resolveTheme(state.active);
      })
      .then((theme) => {
        if (!cancelled && theme) applyCustomTheme(theme);
      })
      .catch(() => {
        // Backend not ready / no persisted theme — keep global.css defaults.
      });
    return () => {
      cancelled = true;
    };
  }, []);
}
