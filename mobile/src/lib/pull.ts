import { useEffect, useRef, useState } from "react";
import { haptic } from "./native";

const THRESHOLD = 72;
const MAX = 110;

/**
 * Pull-to-refresh on a scroll container. Touch-only (there is no hover on a
 * phone); the container must be at scrollTop 0 for a pull to start. Returns
 * the current pull distance (for the indicator) and whether a refresh is in
 * flight.
 */
export function usePullToRefresh<T extends HTMLElement>(
  ref: React.RefObject<T | null>,
  onRefresh: () => Promise<unknown> | unknown,
) {
  const [pull, setPull] = useState(0);
  const [busy, setBusy] = useState(false);
  const startY = useRef<number | null>(null);
  const cb = useRef(onRefresh);
  cb.current = onRefresh;
  const busyRef = useRef(false);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let armed = false;
    let fired = false;

    const onStart = (e: TouchEvent) => {
      if (busyRef.current || el.scrollTop > 0) {
        startY.current = null;
        return;
      }
      startY.current = e.touches[0].clientY;
      armed = false;
      fired = false;
    };
    const onMove = (e: TouchEvent) => {
      if (startY.current === null || busyRef.current) return;
      const dy = e.touches[0].clientY - startY.current;
      if (dy <= 0 || el.scrollTop > 0) {
        if (armed) setPull(0);
        armed = false;
        return;
      }
      armed = true;
      // Rubber-band.
      const d = Math.min(MAX, dy * 0.55);
      setPull(d);
      if (d >= THRESHOLD && !fired) {
        fired = true;
        haptic("light");
      } else if (d < THRESHOLD && fired) {
        fired = false;
      }
    };
    const onEnd = async () => {
      if (startY.current === null) return;
      startY.current = null;
      if (!armed) return;
      if (fired) {
        busyRef.current = true;
        setBusy(true);
        setPull(THRESHOLD * 0.7);
        try {
          await cb.current();
        } finally {
          busyRef.current = false;
          setBusy(false);
          setPull(0);
        }
      } else {
        setPull(0);
      }
      armed = false;
    };

    el.addEventListener("touchstart", onStart, { passive: true });
    el.addEventListener("touchmove", onMove, { passive: true });
    el.addEventListener("touchend", onEnd, { passive: true });
    el.addEventListener("touchcancel", onEnd, { passive: true });
    return () => {
      el.removeEventListener("touchstart", onStart);
      el.removeEventListener("touchmove", onMove);
      el.removeEventListener("touchend", onEnd);
      el.removeEventListener("touchcancel", onEnd);
    };
  }, [ref]);

  return { pull, busy, ready: pull >= THRESHOLD };
}
