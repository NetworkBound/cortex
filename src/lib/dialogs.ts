import { create } from "zustand";

/**
 * In-app replacement for `window.confirm` / `window.prompt`.
 *
 * Native chrome dialogs ignore the theme/token system entirely and read as
 * unfinished in a Tauri app, so every call site routes through this store
 * instead and `<DialogHost/>` (mounted once in App.tsx) renders the active
 * request as a token-styled modal.
 *
 * Same shape as `lib/toast.ts`: a zustand store plus module-scope helpers so
 * non-React code (slash commands, store actions) can call it too. The helpers
 * keep the native semantics — `confirmDialog` resolves `false` on
 * cancel/Escape/backdrop, `promptDialog` resolves `null` on cancel and the
 * (possibly empty) string on OK — so call sites stay a mechanical
 * `window.confirm(x)` → `await confirmDialog({ message: x })` rewrite.
 */

export interface ConfirmDialogArgs {
  /** Heading. Defaults to "Are you sure?". */
  title?: string;
  /** Body copy. Newlines render as separate paragraphs. */
  message: string;
  /** Confirm button label. Defaults to "Confirm". */
  confirmLabel?: string;
  /** Cancel button label. Defaults to "Cancel". */
  cancelLabel?: string;
  /**
   * Destructive action: confirm button renders danger-filled and initial
   * focus lands on Cancel so a stray Enter can't destroy anything.
   */
  danger?: boolean;
}

export interface PromptDialogArgs {
  /** Heading. Defaults to "Enter a value". */
  title?: string;
  /** Label above the input. */
  message: string;
  placeholder?: string;
  /** Pre-filled input value (window.prompt's second arg). */
  initialValue?: string;
  /** Confirm button label. Defaults to "OK". */
  confirmLabel?: string;
  cancelLabel?: string;
}

export type DialogRequest =
  | { id: string; kind: "confirm"; args: ConfirmDialogArgs; resolve: (v: boolean) => void }
  | { id: string; kind: "prompt"; args: PromptDialogArgs; resolve: (v: string | null) => void };

interface DialogState {
  /** FIFO — DialogHost renders queue[0]; sequential awaits queue naturally. */
  queue: DialogRequest[];
  push: (req: DialogRequest) => void;
  /** Resolve + remove the request. DialogHost calls this exactly once. */
  settle: (id: string, value: boolean | string | null) => void;
}

export const useDialogStore = create<DialogState>((set, get) => ({
  queue: [],
  push: (req) => set((s) => ({ queue: [...s.queue, req] })),
  settle: (id, value) => {
    const req = get().queue.find((r) => r.id === id);
    if (!req) return;
    set((s) => ({ queue: s.queue.filter((r) => r.id !== id) }));
    if (req.kind === "confirm") req.resolve(value === true);
    else req.resolve(typeof value === "string" ? value : null);
  },
}));

/** In-app `window.confirm`: resolves true on confirm, false otherwise. */
export function confirmDialog(args: ConfirmDialogArgs): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    useDialogStore.getState().push({
      id: `dlg-${crypto.randomUUID()}`,
      kind: "confirm",
      args,
      resolve,
    });
  });
}

/** In-app `window.prompt`: resolves the entered string, or null on cancel. */
export function promptDialog(args: PromptDialogArgs): Promise<string | null> {
  return new Promise<string | null>((resolve) => {
    useDialogStore.getState().push({
      id: `dlg-${crypto.randomUUID()}`,
      kind: "prompt",
      args,
      resolve,
    });
  });
}

// E2E/devtools handle: lets the headless runner open a dialog and assert the
// rendered modal without driving a deep panel flow first.
declare global {
  interface Window {
    __cortexDialogs?: {
      confirm: typeof confirmDialog;
      prompt: typeof promptDialog;
    };
  }
}
if (typeof window !== "undefined") {
  window.__cortexDialogs = { confirm: confirmDialog, prompt: promptDialog };
}
