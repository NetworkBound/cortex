/**
 * UI-state persistence for Cortex.
 *
 * Persists a small slice of the Zustand store (activity tab + current worktree
 * selection) into `localStorage["cortex.ui"]` so the app boots back into the
 * user's last layout. Other localStorage keys already in use elsewhere:
 *   - `cortex.theme`         (src/lib/themes.ts)
 *   - `cortex.watchMode`     (watch-mode feature flag)
 *   - `cortex.watchModeRoots`
 *
 * Design notes
 * - We only persist UI presentation state — not session data, messages, or
 *   secrets. Keep this surface tiny so a future schema bump is cheap.
 * - Ephemeral fields (showQuickOpen / showSettings / showCommandPalette) are
 *   intentionally NOT persisted: a modal/palette reopening on boot is
 *   surprising UX.
 * - All localStorage access is wrapped in try/catch: private-browsing /
 *   Tauri-without-storage / quota errors must never break the app.
 * - `attachUIStatePersistence` uses a Zustand subscription with a debounce so
 *   rapid state churn (e.g. opening/closing the activity panel) coalesces into
 *   a single write. Zustand v5's `subscribe(listener)` invokes the listener
 *   with the full state on every change, so we snapshot + deep-equal before
 *   scheduling a write.
 */
import { useCortexStore, type ActivityTab } from "@/state/store";

const STORAGE_KEY = "cortex.ui";

const ACTIVITY_TABS: readonly Exclude<ActivityTab, null>[] = [
  "brain",
  "memory",
  "sessions",
  "projects",
  "graph",
  "agents",
  "usage",
  "observability",
];

export interface PersistedUIState {
  activityTab: ActivityTab;
  currentWorktreeId: string | null;
  currentWorktreePath: string | null;
}

function isActivityTab(value: unknown): value is ActivityTab {
  if (value === null) return true;
  return (
    typeof value === "string" && (ACTIVITY_TABS as readonly string[]).includes(value)
  );
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function sanitize(raw: unknown): PersistedUIState | null {
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  if (!isActivityTab(obj.activityTab)) return null;
  if (!isNullableString(obj.currentWorktreeId)) return null;
  if (!isNullableString(obj.currentWorktreePath)) return null;
  return {
    activityTab: obj.activityTab,
    currentWorktreeId: obj.currentWorktreeId,
    currentWorktreePath: obj.currentWorktreePath,
  };
}

/**
 * Write the given UI state to localStorage. Failures are swallowed because
 * localStorage may be unavailable (private mode, restricted Tauri runtime,
 * quota exhaustion). Persistence is a nice-to-have, not load-bearing.
 */
export function saveUIState(state: PersistedUIState): void {
  try {
    if (typeof localStorage === "undefined") return;
    const payload: PersistedUIState = {
      activityTab: state.activityTab,
      currentWorktreeId: state.currentWorktreeId,
      currentWorktreePath: state.currentWorktreePath,
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // ignore — see docstring
  }
}

/**
 * Read the persisted UI state. Returns `null` if nothing is stored, the value
 * fails to parse, or it doesn't match the expected shape. The shape check is
 * important — a stale schema from a previous build shouldn't crash boot.
 */
export function loadUIState(): PersistedUIState | null {
  try {
    if (typeof localStorage === "undefined") return null;
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return sanitize(parsed);
  } catch {
    return null;
  }
}

function equalUIState(a: PersistedUIState, b: PersistedUIState): boolean {
  return (
    a.activityTab === b.activityTab &&
    a.currentWorktreeId === b.currentWorktreeId &&
    a.currentWorktreePath === b.currentWorktreePath
  );
}

/**
 * Subscribe to the Cortex store and persist the UI slice whenever any of the
 * watched fields change. Writes are debounced so a flurry of updates collapses
 * into one localStorage write. Returns the unsubscribe function — call it on
 * teardown (e.g. component unmount, HMR cleanup).
 *
 * Zustand v5's `subscribe(listener)` fires on every set(); we snapshot the
 * three watched fields and compare against the last persisted snapshot to
 * avoid pointless writes when unrelated state (messages, tools, …) changes.
 */
export function attachUIStatePersistence(debounceMs = 300): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: PersistedUIState | null = null;

  const snapshot = (): PersistedUIState => {
    const s = useCortexStore.getState();
    return {
      activityTab: s.activityTab,
      currentWorktreeId: s.currentWorktreeId,
      currentWorktreePath: s.currentWorktreePath,
    };
  };

  const schedule = (next: PersistedUIState) => {
    pending = next;
    if (timer !== null) return;
    timer = setTimeout(() => {
      timer = null;
      if (pending) {
        saveUIState(pending);
        pending = null;
      }
    }, debounceMs);
  };

  let last = snapshot();

  const unsubscribe = useCortexStore.subscribe(() => {
    const next = snapshot();
    if (equalUIState(next, last)) return;
    last = next;
    schedule(next);
  });

  return () => {
    unsubscribe();
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    // Flush any pending write so we don't lose the latest state on teardown.
    if (pending) {
      saveUIState(pending);
      pending = null;
    }
  };
}
