import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Stick-to-bottom auto-scroll that doesn't fight the user: only auto-scroll
 * while they're already near the bottom. Exposes `atBottom` so the view can
 * show a "jump to latest" button, and `jump()` to scroll down on demand.
 */
export function useStickToBottom<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const stick = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  const NEAR = 96;

  const measure = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    const near = distance < NEAR;
    stick.current = near;
    setAtBottom((v) => (v === near ? v : near));
  }, []);

  const notify = useCallback(() => {
    const el = ref.current;
    if (!el || !stick.current) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  const jump = useCallback((smooth = true) => {
    const el = ref.current;
    if (!el) return;
    stick.current = true;
    setAtBottom(true);
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.addEventListener("scroll", measure, { passive: true });
    return () => el.removeEventListener("scroll", measure);
  }, [measure]);

  return { ref, notify, jump, atBottom };
}
