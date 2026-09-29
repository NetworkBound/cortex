/**
 * ContextMenu — a tiny, dependency-free right-click menu.
 *
 * Renders at a fixed viewport position (clamped so it never overflows the
 * window), closes on Escape, outside click, window blur/resize or scroll,
 * and supports Up/Down/Home/End/Enter keyboard navigation. Callers own the
 * open/position state:
 *
 *   const [menu, setMenu] = useState<{ x; y; items } | null>(null);
 *   <div onContextMenu={(e) => { e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY, items }); }} />
 *   {menu && <ContextMenu {...menu} onClose={() => setMenu(null)} />}
 *
 * Styles live in `src/styles/context-menu.css` (token-driven so every theme
 * paints it correctly).
 */

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import "../styles/context-menu.css";

export interface ContextMenuItem {
  /** Stable key (also used for React keys). */
  id: string;
  label: string;
  icon?: ReactNode;
  /** Small right-aligned hint (a shortcut or the target program). */
  hint?: string;
  disabled?: boolean;
  danger?: boolean;
  /** Draw a divider above this item. */
  separatorBefore?: boolean;
  onSelect: () => void | Promise<void>;
}

export interface ContextMenuProps {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
  /** Accessible label for the menu (e.g. the file name). */
  label?: string;
}

const VIEWPORT_MARGIN = 8;

export function ContextMenu({ x, y, items, onClose, label }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  const enabled = items.filter((i) => !i.disabled);
  const [focusIdx, setFocusIdx] = useState(0);

  // Clamp into the viewport once we know the rendered size.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let left = x;
    let top = y;
    if (left + r.width + VIEWPORT_MARGIN > vw)
      left = Math.max(VIEWPORT_MARGIN, vw - r.width - VIEWPORT_MARGIN);
    if (top + r.height + VIEWPORT_MARGIN > vh)
      top = Math.max(VIEWPORT_MARGIN, vh - r.height - VIEWPORT_MARGIN);
    setPos({ left, top });
  }, [x, y, items.length]);

  // Dismissal: outside pointer, Escape, blur, resize, any scroll.
  useEffect(() => {
    const onPointer = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    const close = () => onClose();
    document.addEventListener("mousedown", onPointer, true);
    document.addEventListener("contextmenu", onPointer, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      document.removeEventListener("mousedown", onPointer, true);
      document.removeEventListener("contextmenu", onPointer, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [onClose]);

  // Move focus to the current item so keyboard users can operate the menu.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const buttons = el.querySelectorAll<HTMLButtonElement>(
      "button.context-menu-item:not(:disabled)",
    );
    buttons[focusIdx]?.focus();
  }, [focusIdx, items]);

  function onMenuKey(e: React.KeyboardEvent<HTMLDivElement>) {
    if (enabled.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setFocusIdx((i) => (i + 1) % enabled.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setFocusIdx((i) => (i - 1 + enabled.length) % enabled.length);
    } else if (e.key === "Home") {
      e.preventDefault();
      setFocusIdx(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setFocusIdx(enabled.length - 1);
    }
  }

  async function pick(item: ContextMenuItem) {
    if (item.disabled) return;
    onClose();
    try {
      await item.onSelect();
    } catch {
      /* item handlers surface their own errors (toasts) */
    }
  }

  return (
    <div
      ref={ref}
      className="context-menu"
      role="menu"
      aria-label={label}
      style={{ left: pos.left, top: pos.top }}
      onKeyDown={onMenuKey}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item) => (
        <div key={item.id} className="context-menu-entry">
          {item.separatorBefore && (
            <div className="context-menu-sep" role="separator" />
          )}
          <button
            type="button"
            role="menuitem"
            className={`context-menu-item${item.danger ? " danger" : ""}`}
            disabled={item.disabled}
            onClick={() => void pick(item)}
          >
            {item.icon && (
              <span className="context-menu-icon" aria-hidden="true">
                {item.icon}
              </span>
            )}
            <span className="context-menu-label">{item.label}</span>
            {item.hint && (
              <span className="context-menu-hint">{item.hint}</span>
            )}
          </button>
        </div>
      ))}
    </div>
  );
}
