import {
  useCallback,
  useEffect,
  useRef,
  type ReactNode,
} from "react";

// Shared modal primitive. Promotes the focus-management + Escape-to-close +
// role="dialog"/aria-modal behavior that DialogHost (lib/dialogs) and the
// various bespoke `.modal-backdrop` overlays each reimplemented (often
// incompletely — most lacked a focus trap, so Tab walked out of the modal into
// the obscured app behind it). Use this for new modals; existing ones can be
// migrated incrementally.
//
// Styling reuses the existing token-styled `.dialog-backdrop` / `.dialog-card`
// chrome so it matches DialogHost out of the box. Pass `className` to layer on
// a per-modal class (e.g. wider cards).

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "textarea:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  /** Accessible name for the dialog. Rendered as the title row unless
   *  `hideTitle` is set; always wired to `aria-label`. */
  title?: string;
  /** Hide the visible title row but keep it as the accessible name. */
  hideTitle?: boolean;
  /** "dialog" (default) or "alertdialog" for destructive confirmations. */
  role?: "dialog" | "alertdialog";
  /** Extra class on the card (e.g. for a wider layout). */
  className?: string;
  /** Click on the backdrop closes the modal (default true). */
  closeOnBackdrop?: boolean;
  /** Footer actions row, rendered below the body in the actions bar. */
  footer?: ReactNode;
  children?: ReactNode;
}

export function Modal({
  open,
  onClose,
  title,
  hideTitle = false,
  role = "dialog",
  className,
  closeOnBackdrop = true,
  footer,
  children,
}: ModalProps) {
  const cardRef = useRef<HTMLDivElement>(null);
  // The element focused before the modal opened, restored on close so keyboard
  // focus returns where the user left it (a11y requirement for dialogs).
  const restoreRef = useRef<HTMLElement | null>(null);

  // Escape closes. Capture phase so global shortcut handlers underneath never
  // see the Escape that dismisses the modal (matches DialogHost's behavior).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  // Focus management: stash the previously-focused element, move focus into the
  // card, and restore on close.
  useEffect(() => {
    if (!open) return;
    restoreRef.current = (document.activeElement as HTMLElement | null) ?? null;
    const card = cardRef.current;
    if (card) {
      const first = card.querySelector<HTMLElement>(FOCUSABLE);
      (first ?? card).focus();
    }
    return () => {
      restoreRef.current?.focus?.();
    };
  }, [open]);

  // Trap Tab within the card so keyboard focus can't escape into the obscured
  // app behind the modal.
  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab") return;
    const card = cardRef.current;
    if (!card) return;
    const items = Array.from(card.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null || el === document.activeElement,
    );
    if (items.length === 0) {
      e.preventDefault();
      card.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }, []);

  if (!open) return null;

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={closeOnBackdrop ? onClose : undefined}
    >
      <div
        ref={cardRef}
        className={className ? `dialog-card ${className}` : "dialog-card"}
        role={role}
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        {title && !hideTitle && <div className="dialog-title">{title}</div>}
        {children}
        {footer && <div className="dialog-actions">{footer}</div>}
      </div>
    </div>
  );
}
