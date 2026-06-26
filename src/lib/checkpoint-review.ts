import { create } from "zustand";
import {
  diffCheckpoint,
  type CheckpointDiff,
  type CheckpointInfo,
} from "@/lib/checkpoints";

/**
 * Global "review a checkpoint before restoring" modal, callable from non-React
 * code (slash commands, store actions) — the same way `lib/dialogs.ts` exposes
 * `confirmDialog`/`promptDialog`.
 *
 * The Checkpoints panel already shows a pre-restore diff inline (it owns its own
 * modal state), but `/undo` ran from chat had no UI surface to hang a modal off,
 * so it force-restored the most-recent snapshot sight-unseen. This host gives
 * `/undo` (and any other off-panel caller) the same read-only diff review the
 * panel offers: compute `diff_checkpoint`, render `<CheckpointDiffModal/>` (via
 * `<CheckpointReviewHost/>`, mounted once in App), and only restore on confirm.
 *
 * Nothing is mutated until the user confirms — the host owns the actual restore
 * so callers just await the outcome.
 */

export type CheckpointReviewOutcome =
  | { outcome: "restored" }
  | { outcome: "cancelled" }
  | { outcome: "error"; message: string };

export interface CheckpointReviewRequest {
  id: string;
  root: string;
  checkpoint: CheckpointInfo;
  diff: CheckpointDiff;
  resolve: (r: CheckpointReviewOutcome) => void;
}

interface CheckpointReviewState {
  /** At most one review is open at a time (restore is a singular action). */
  active: CheckpointReviewRequest | null;
  open: (req: CheckpointReviewRequest) => void;
  /** Resolve the active request and dismiss the modal. Called once by the host. */
  settle: (id: string, value: CheckpointReviewOutcome) => void;
}

export const useCheckpointReviewStore = create<CheckpointReviewState>((set, get) => ({
  active: null,
  open: (req) => {
    // If a review is somehow already open, cancel it before replacing so its
    // awaiting caller isn't left hanging forever.
    const prev = get().active;
    if (prev) prev.resolve({ outcome: "cancelled" });
    set({ active: req });
  },
  settle: (id, value) => {
    const cur = get().active;
    if (!cur || cur.id !== id) return;
    set({ active: null });
    cur.resolve(value);
  },
}));

/**
 * Open the read-only restore-preview modal for `checkpoint` and resolve once the
 * user confirms (after the restore runs), cancels, or the restore errors.
 *
 * Throws only if the diff itself can't be computed (e.g. the tarball is gone) —
 * callers should wrap in try/catch and surface that as an undo failure.
 */
export async function reviewCheckpointRestore(
  root: string,
  checkpoint: CheckpointInfo,
): Promise<CheckpointReviewOutcome> {
  const diff = await diffCheckpoint(root, checkpoint.id);
  return new Promise<CheckpointReviewOutcome>((resolve) => {
    useCheckpointReviewStore.getState().open({
      id: `ckr-${crypto.randomUUID()}`,
      root,
      checkpoint,
      diff,
      resolve,
    });
  });
}

// E2E/devtools handle: lets the headless runner open the review modal with a
// synthetic checkpoint+diff and assert it paints, without needing a real
// project + checkpoint tarball on disk first. Mirrors `window.__cortexDialogs`.
declare global {
  interface Window {
    __cortexCheckpointReview?: {
      /** Drive the full helper (computes the diff via the backend). */
      review: typeof reviewCheckpointRestore;
      /** Inject a request directly — render-only, no backend, no restore. */
      openWith: (checkpoint: CheckpointInfo, diff: CheckpointDiff) => Promise<CheckpointReviewOutcome>;
    };
  }
}
if (typeof window !== "undefined") {
  window.__cortexCheckpointReview = {
    review: reviewCheckpointRestore,
    openWith: (checkpoint, diff) =>
      new Promise<CheckpointReviewOutcome>((resolve) => {
        useCheckpointReviewStore.getState().open({
          id: `ckr-e2e-${crypto.randomUUID()}`,
          root: "",
          checkpoint,
          diff,
          resolve,
        });
      }),
  };
}
