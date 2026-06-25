import { useCallback, useEffect, useRef } from "react";

/**
 * Stick-to-bottom auto-scroll that doesn't fight the user. We only auto-scroll
 * when the user is already near the bottom; if they scroll up to read, we leave
 * them alone until they return to the bottom.
 *
 * Returns the scroll-container ref and a `notify` to call whenever content
 * changes (new token, new message).
 */
export function useStickToBottom<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const stick = useRef(true);
  const NEAR = 80; // px from bottom counts as "at bottom"

  const onScroll = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    stick.current = distance < NEAR;
  }, []);

  const notify = useCallback(() => {
    const el = ref.current;
    if (!el || !stick.current) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [onScroll]);

  return { ref, notify };
}
