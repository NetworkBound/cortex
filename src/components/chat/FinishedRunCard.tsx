import { useMemo } from "react";
import { useCortexStore, type Message } from "@/state/store";
import { findCommand, makeContext } from "@/lib/slash-commands";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { playSound } from "@/lib/sounds";
import { describeRun, summarizeRun } from "./run-summary";

/**
 * "Done — n edits · m tools" footer under the most recent finished assistant
 * turn, with the follow-ups a reviewer actually wants next:
 *
 *   Diff    → the pending-edit review modal when this session recorded edits,
 *             otherwise the Source Control tab for the project
 *   Review  → the shipped `/review` slash command (AI review by another model)
 *   Replay  → Run Replay in Observability, focused on THIS run
 *             (same deep-link IssuesPanel uses: `replayFocusSpanId`)
 *   Copy    → the turn's text
 *
 * Only rendered by MessageList for the latest completed turn that has a
 * `runId`; earlier turns keep the hover-only MessageActions toolbar.
 */
export function FinishedRunCard({ message }: { message: Message }) {
  const hasProject = useCortexStore((s) => s.activeProject !== null);
  const summary = useMemo(() => summarizeRun(message), [message]);
  const runId = message.runId;
  if (!runId) return null;

  const openDiff = () => {
    const st = useCortexStore.getState();
    // Edits recorded this session → the Composer review modal (per-file
    // accept/reject). Otherwise the repo-level view is the honest answer.
    if (st.composerEdits.length > 0) st.setShowComposer(true);
    else st.setActivityTab("source-control");
  };

  const openReview = () => {
    const cmd = findCommand("/review");
    if (!cmd) {
      pushToast({ title: "/review is unavailable", kind: "warning" });
      return;
    }
    // The command reports "no active project" itself.
    void cmd.run("", makeContext());
  };

  const openReplay = () => {
    const st = useCortexStore.getState();
    st.setReplayFocusSpanId(runId);
    st.setActivityTab("observability");
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(message.content);
      pushToast({ title: "copied", kind: "success", ttlMs: 1800 });
      playSound("tick");
    } catch (e) {
      pushToast({
        title: "copy failed",
        body: humanizeError(e),
        kind: "error",
      });
    }
  };

  return (
    <div
      className={`run-card${summary.toolErrors > 0 ? " run-card--errors" : ""}`}
      role="group"
      aria-label="finished run"
      data-run-id={runId}
    >
      <span className="run-card-summary">
        <span className="run-card-dot" aria-hidden="true" />
        Done — {describeRun(summary)}
        {message.totalTokens ? (
          <span className="muted"> · {message.totalTokens} tokens</span>
        ) : null}
      </span>
      <span className="run-card-actions">
        {hasProject && (
          <button
            type="button"
            className="action-btn"
            onClick={openDiff}
            title={
              summary.edits > 0
                ? "Review the files this run changed"
                : "Open Source Control for this project"
            }
          >
            Diff
          </button>
        )}
        {hasProject && (
          <button
            type="button"
            className="action-btn"
            onClick={openReview}
            title="AI code review of the uncommitted changes (/review)"
          >
            Review
          </button>
        )}
        <button
          type="button"
          className="action-btn"
          onClick={openReplay}
          title="Open Run Replay focused on this run"
        >
          Replay
        </button>
        <button
          type="button"
          className="action-btn"
          onClick={() => void copy()}
          title="Copy this turn's text"
        >
          Copy
        </button>
      </span>
    </div>
  );
}
