// Small shared building blocks: sub-screen header, pull-to-refresh scroller,
// bottom sheets, toasts, skeletons, empty states, list rows (with long-press
// and swipe), chips, toggles, banners.

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type ReactNode,
} from "react";
import Icon from "./Icon";
import { usePullToRefresh } from "../lib/pull";
import { haptic } from "../lib/native";
import { useStore } from "../lib/store";
import { back } from "../lib/nav";

// ── Header for pushed screens ──────────────────────────────────────────────

export function SubHeader({
  title,
  sub,
  right,
  onBack,
}: {
  title: ReactNode;
  sub?: ReactNode;
  right?: ReactNode;
  onBack?: () => void;
}) {
  return (
    <header className="subheader">
      <button
        className="iconbtn"
        onClick={onBack ?? (() => back())}
        aria-label="Back"
      >
        <Icon name="back" />
      </button>
      <div className="subheader-title">
        <div className="t">{title}</div>
        {sub && <div className="s">{sub}</div>}
      </div>
      <div className="subheader-right">{right}</div>
    </header>
  );
}

// ── Scroll container with pull-to-refresh ──────────────────────────────────

export const Scroll = forwardRef<
  HTMLDivElement,
  {
    onRefresh?: () => Promise<unknown> | unknown;
    className?: string;
    children: ReactNode;
  }
>(function Scroll({ onRefresh, className, children }, fwd) {
  const ref = useRef<HTMLDivElement | null>(null);
  useImperativeHandle(fwd, () => ref.current as HTMLDivElement);
  const { pull, busy, ready } = usePullToRefresh(ref, onRefresh ?? (() => {}));
  const active = !!onRefresh && (pull > 0 || busy);
  return (
    <div className={`scroll ${className ?? ""}`} ref={ref}>
      {onRefresh && (
        <div
          className={`ptr ${busy ? "busy" : ""} ${ready ? "ready" : ""}`}
          style={{ height: active ? pull : 0 }}
          aria-hidden={!active}
        >
          <span className="spin" />
        </div>
      )}
      {children}
    </div>
  );
});

// ── Sheets ─────────────────────────────────────────────────────────────────

export function Sheet({
  open,
  onClose,
  title,
  children,
  tall,
}: {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  children: ReactNode;
  tall?: boolean;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div
        className={`sheet ${tall ? "tall" : ""}`}
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="sheet-grab" />
        {title && <div className="sheet-title">{title}</div>}
        <div className="sheet-body">{children}</div>
      </div>
    </div>
  );
}

export interface Action {
  label: string;
  icon?: string;
  destructive?: boolean;
  disabled?: boolean;
  onClick: () => void;
}

export function ActionSheet({
  open,
  onClose,
  title,
  actions,
}: {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  actions: Action[];
}) {
  return (
    <Sheet open={open} onClose={onClose} title={title}>
      <div className="actions-list">
        {actions.map((a) => (
          <button
            key={a.label}
            className={`action-row ${a.destructive ? "destructive" : ""}`}
            disabled={a.disabled}
            onClick={() => {
              onClose();
              a.onClick();
            }}
          >
            {a.icon && <Icon name={a.icon} size={20} />}
            {a.label}
          </button>
        ))}
        <button className="action-row cancel" onClick={onClose}>
          Cancel
        </button>
      </div>
    </Sheet>
  );
}

/** Long-press-to-confirm button for destructive actions (checkpoint restore,
 *  revoke). Fires `onConfirm` after holding ~700 ms. */
export function HoldButton({
  label,
  holding,
  onConfirm,
  className,
}: {
  label: string;
  holding: string;
  onConfirm: () => void;
  className?: string;
}) {
  const [down, setDown] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clear = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setDown(false);
  };
  const start = () => {
    setDown(true);
    haptic("light");
    timer.current = setTimeout(() => {
      clear();
      haptic("success");
      onConfirm();
    }, 700);
  };
  return (
    <button
      className={`btn hold ${down ? "holding" : ""} ${className ?? ""}`}
      onPointerDown={start}
      onPointerUp={clear}
      onPointerLeave={clear}
      onPointerCancel={clear}
      onContextMenu={(e) => e.preventDefault()}
    >
      <span className="hold-fill" />
      <span className="hold-label">{down ? holding : label}</span>
    </button>
  );
}

// ── Toasts ─────────────────────────────────────────────────────────────────

export function Toasts() {
  const { toasts, dismissToast } = useStore();
  if (toasts.length === 0) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <button
          key={t.id}
          className={`toast ${t.kind}`}
          onClick={() => dismissToast(t.id)}
        >
          {t.text}
        </button>
      ))}
    </div>
  );
}

// ── Loading / empty ────────────────────────────────────────────────────────

export function Skeleton({
  rows = 4,
  lines = 2,
}: {
  rows?: number;
  lines?: number;
}) {
  return (
    <div className="skeleton" aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }).map((_, i) => (
        <div className="sk-row" key={i}>
          <div className="sk-line w60" />
          {lines > 1 && <div className="sk-line w90 thin" />}
        </div>
      ))}
    </div>
  );
}

export function Empty({
  icon,
  title,
  hint,
  action,
}: {
  icon?: string;
  title: ReactNode;
  hint?: ReactNode;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="empty">
      {icon && <Icon name={icon} size={34} className="empty-ico" />}
      <div className="empty-title">{title}</div>
      {hint && <div className="empty-hint">{hint}</div>}
      {action && (
        <button className="btn primary" onClick={action.onClick}>
          {action.label}
        </button>
      )}
    </div>
  );
}

export function Banner({
  kind = "info",
  children,
  action,
}: {
  kind?: "info" | "error" | "warn" | "success";
  children: ReactNode;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div
      className={`banner ${kind}`}
      role={kind === "error" ? "alert" : undefined}
    >
      <span className="banner-text">{children}</span>
      {action && (
        <button className="btn small" onClick={action.onClick}>
          {action.label}
        </button>
      )}
    </div>
  );
}

// ── Rows ───────────────────────────────────────────────────────────────────

const LONG_MS = 480;
const SWIPE_PX = 64;

export function useLongPress(cb: (() => void) | undefined) {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fired = useRef(false);
  const origin = useRef<{ x: number; y: number } | null>(null);

  const cancel = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    origin.current = null;
  }, []);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!cb) return;
      fired.current = false;
      origin.current = { x: e.clientX, y: e.clientY };
      timer.current = setTimeout(() => {
        fired.current = true;
        haptic("medium");
        cb();
      }, LONG_MS);
    },
    [cb],
  );
  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      const o = origin.current;
      if (!o) return;
      if (Math.abs(e.clientX - o.x) > 10 || Math.abs(e.clientY - o.y) > 10) {
        cancel();
      }
    },
    [cancel],
  );
  const onClickCapture = useCallback((e: React.MouseEvent) => {
    if (fired.current) {
      e.stopPropagation();
      e.preventDefault();
      fired.current = false;
    }
  }, []);

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp: cancel,
    onPointerCancel: cancel,
    onPointerLeave: cancel,
    onClickCapture,
    onContextMenu: (e: React.MouseEvent) => cb && e.preventDefault(),
  };
}

export function Row({
  leading,
  title,
  sub,
  right,
  onClick,
  onLongPress,
  onSwipeLeft,
  chevron,
  selected,
  className,
}: {
  leading?: ReactNode;
  title: ReactNode;
  sub?: ReactNode;
  right?: ReactNode;
  onClick?: () => void;
  onLongPress?: () => void;
  onSwipeLeft?: () => void;
  chevron?: boolean;
  selected?: boolean;
  className?: string;
}) {
  const lp = useLongPress(onLongPress);
  const touch = useRef<{ x: number; y: number } | null>(null);
  const [dx, setDx] = useState(0);

  const onTouchStart = (e: React.TouchEvent) => {
    if (!onSwipeLeft) return;
    touch.current = { x: e.touches[0].clientX, y: e.touches[0].clientY };
  };
  const onTouchMove = (e: React.TouchEvent) => {
    const t = touch.current;
    if (!t) return;
    const ddx = e.touches[0].clientX - t.x;
    const ddy = e.touches[0].clientY - t.y;
    if (Math.abs(ddy) > Math.abs(ddx)) {
      touch.current = null;
      setDx(0);
      return;
    }
    if (ddx < 0) setDx(Math.max(-SWIPE_PX - 20, ddx));
  };
  const onTouchEnd = () => {
    if (!touch.current) return;
    touch.current = null;
    if (dx <= -SWIPE_PX) {
      haptic("light");
      onSwipeLeft?.();
    }
    setDx(0);
  };

  return (
    <div
      className={`row-wrap ${className ?? ""}`}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onTouchCancel={onTouchEnd}
    >
      {onSwipeLeft && (
        <div className="row-swipe-hint" aria-hidden="true">
          <Icon name="more" size={18} />
        </div>
      )}
      <button
        type="button"
        className={`row ${selected ? "selected" : ""}`}
        style={dx ? { transform: `translateX(${dx}px)` } : undefined}
        onClick={onClick}
        {...lp}
      >
        {leading && <div className="row-lead">{leading}</div>}
        <div className="row-meta">
          <div className="row-title">{title}</div>
          {sub && <div className="row-sub">{sub}</div>}
        </div>
        {right && <div className="row-right">{right}</div>}
        {chevron && <Icon name="chevron" size={18} className="row-chev" />}
      </button>
    </div>
  );
}

export function GroupHead({ children }: { children: ReactNode }) {
  return <div className="group-head">{children}</div>;
}

export function Chip({
  children,
  tone,
  className,
}: {
  children: ReactNode;
  tone?: "ok" | "warn" | "err" | "info" | "accent" | "muted";
  className?: string;
}) {
  return (
    <span className={`chip ${tone ?? ""} ${className ?? ""}`}>{children}</span>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className={`toggle ${checked ? "on" : ""}`}
      disabled={disabled}
      onClick={() => {
        haptic("selection");
        onChange(!checked);
      }}
    >
      <span className="knob" />
    </button>
  );
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="segmented" role="tablist">
      {options.map((o) => (
        <button
          key={o.value}
          role="tab"
          aria-selected={value === o.value}
          className={value === o.value ? "active" : ""}
          onClick={() => {
            haptic("selection");
            onChange(o.value);
          }}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Spinner({ small }: { small?: boolean }) {
  return <span className={`spin ${small ? "small" : ""}`} aria-hidden="true" />;
}
