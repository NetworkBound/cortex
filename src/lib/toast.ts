import { create } from "zustand";

export type ToastKind = "info" | "success" | "error" | "warning";

/** Optional inline action button rendered on the card ("Open", "Undo"…).
 *  Clicking it runs `onClick` and dismisses the toast. */
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  title: string;
  body?: string;
  kind: ToastKind;
  ttlMs: number;
  createdAt: number;
  action?: ToastAction;
}

export interface PushToastArgs {
  title: string;
  body?: string;
  kind?: ToastKind;
  ttlMs?: number;
  action?: ToastAction;
}

interface ToastState {
  toasts: Toast[];
  pushToast: (args: PushToastArgs) => string;
  dismissToast: (id: string) => void;
}

const DEFAULT_KIND: ToastKind = "info";
const DEFAULT_TTL_MS = 4000;

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  pushToast: ({ title, body, kind, ttlMs, action }) => {
    const resolvedKind = kind ?? DEFAULT_KIND;
    // Dedup: a repeatedly-firing source (e.g. the connectivity poller while
    // the gateway is offline) would otherwise stack dozens of identical
    // cards. If one with the same title/body/kind is already showing, reuse
    // it instead of piling on a duplicate.
    const existing = get().toasts.find(
      (t) => t.title === title && t.body === body && t.kind === resolvedKind,
    );
    if (existing) return existing.id;
    const id = `t-${crypto.randomUUID()}`;
    const toast: Toast = {
      id,
      title,
      body,
      kind: resolvedKind,
      ttlMs: ttlMs ?? DEFAULT_TTL_MS,
      createdAt: Date.now(),
      action,
    };
    set((s) => ({ toasts: [...s.toasts, toast] }));
    return id;
  },
  dismissToast: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

/**
 * Convenience helper for non-React call sites that need to push a toast
 * without subscribing to the store.
 */
export function pushToast(args: PushToastArgs): string {
  return useToastStore.getState().pushToast(args);
}
