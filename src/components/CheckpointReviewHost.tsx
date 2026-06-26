import { useEffect, useState } from "react";
import { restoreCheckpoint } from "@/lib/checkpoints";
import { useCheckpointReviewStore } from "@/lib/checkpoint-review";
import { humanizeError } from "@/lib/errors";
import { CheckpointDiffModal } from "./CheckpointDiffModal";

/**
 * Renders the global checkpoint restore-preview modal (driven by
 * `lib/checkpoint-review.ts`) so off-panel callers like `/undo` get the same
 * read-only diff review the Checkpoints panel offers. Mounted once in App.tsx
 * next to `<DialogHost/>`.
 *
 * Owns the actual restore: the diff was computed read-only by the caller, and
 * nothing touches the tree until the user confirms here.
 */
export function CheckpointReviewHost() {
  const active = useCheckpointReviewStore((s) => s.active);
  const settle = useCheckpointReviewStore((s) => s.settle);
  const [restoring, setRestoring] = useState(false);

  // Reset the in-flight flag whenever a new (or no) request becomes active so a
  // prior restore's state can't leak into the next review.
  useEffect(() => {
    setRestoring(false);
  }, [active?.id]);

  if (!active) return null;

  return (
    <CheckpointDiffModal
      checkpoint={active.checkpoint}
      diff={active.diff}
      restoring={restoring}
      onCancel={() => {
        if (!restoring) settle(active.id, { outcome: "cancelled" });
      }}
      onConfirm={async () => {
        setRestoring(true);
        try {
          await restoreCheckpoint(active.root, active.checkpoint.id, true);
          settle(active.id, { outcome: "restored" });
        } catch (e) {
          settle(active.id, { outcome: "error", message: humanizeError(e) });
        }
      }}
    />
  );
}
