/**
 * Shared "architecture tab open" flag.
 *
 * The architecture panel is a NEW activity surface, but the `ActivityTab`
 * union (state/store.ts) and ACTIVITY_ICONS (lib/activity-icons.tsx) are owned
 * by other modules, so we cannot extend them. Instead this tiny external store
 * lets ActivityBar (the pill) and ActivityPanel (the render branch) coordinate
 * the open/closed state independently of the global `activityTab`. Opening the
 * arch tab also clears the global tab so only one panel shows at a time.
 *
 * Lives in lib/ (not in ArchitectureView.tsx) so the always-mounted shell can
 * read the flag without statically pulling the diagram view — and its CSS —
 * into the startup bundle; the view itself is React.lazy'd by ActivityPanel.
 */
import { useSyncExternalStore } from "react";
import { useCortexStore } from "@/state/store";

let archOpen = false;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export const archTab = {
  isOpen: () => archOpen,
  open() {
    if (archOpen) return;
    archOpen = true;
    // Close any global activity tab so the panels don't stack.
    useCortexStore.getState().setActivityTab(null);
    emit();
  },
  close() {
    if (!archOpen) return;
    archOpen = false;
    emit();
  },
  toggle() {
    if (archOpen) this.close();
    else this.open();
  },
  subscribe(fn: () => void) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  },
};

/** React hook: subscribe to the architecture-tab open flag. */
export function useArchTabOpen(): boolean {
  return useSyncExternalStore(
    (cb) => archTab.subscribe(cb),
    () => archOpen,
    () => archOpen,
  );
}
