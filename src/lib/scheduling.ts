/**
 * Small scheduling helpers for startup and background work.
 *
 * - `runWhenIdle` defers non-critical boot work (listener wiring, background
 *   pulls) until after the first frames have painted, so the shell shows up
 *   before we spend main-thread time on things the user can't see yet.
 * - `visibleInterval` is `setInterval` that goes quiet while the window is
 *   hidden (minimised / on another virtual desktop), then catches up with one
 *   immediate tick when the window is shown again. Polling panels use it so a
 *   backgrounded Cortex doesn't keep hammering the backend every few seconds.
 *
 * Both are DOM-only (no React) so lib code and components can share them.
 */

type IdleWindow = Window & {
  requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number;
  cancelIdleCallback?: (id: number) => void;
};

/**
 * Run `fn` once the browser is idle, or after `timeoutMs` at the latest.
 * Falls back to a short `setTimeout` where `requestIdleCallback` is missing
 * (older WebKitGTK builds). Returns a cancel function.
 */
export function runWhenIdle(fn: () => void, timeoutMs = 1_000): () => void {
  const w = window as IdleWindow;
  if (typeof w.requestIdleCallback === "function") {
    const id = w.requestIdleCallback(fn, { timeout: timeoutMs });
    return () => w.cancelIdleCallback?.(id);
  }
  const id = window.setTimeout(fn, Math.min(timeoutMs, 50));
  return () => window.clearTimeout(id);
}

/**
 * `setInterval` that pauses while `document.visibilityState === "hidden"`.
 * A tick that lands while hidden is remembered and replayed as soon as the
 * page becomes visible again, so the panel is fresh the moment it's back on
 * screen instead of up to one period stale. Returns a stop function.
 */
export function visibleInterval(fn: () => void, ms: number): () => void {
  let missed = false;
  const tick = () => {
    if (document.visibilityState === "hidden") {
      missed = true;
      return;
    }
    missed = false;
    fn();
  };
  const id = window.setInterval(tick, ms);
  const onVisibility = () => {
    if (document.visibilityState === "visible" && missed) {
      missed = false;
      fn();
    }
  };
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    window.clearInterval(id);
    document.removeEventListener("visibilitychange", onVisibility);
  };
}
