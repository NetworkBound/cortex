import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/backup";
import { useCortexStore } from "@/state/store";
import {
  deleteLaneRun,
  laneReview,
  listProviders,
  listLaneRuns,
  mergeLaneRun,
  reattachLaneRun,
  runProviderLanes,
  stopLaneRun,
  type LaneReview,
  type LaneRunRecord,
} from "@/lib/multi-provider";

/**
 * Parallel providers on one project, worktree-isolated.
 *
 * Pick a Gitea project + several providers (ChatGPT, Claude, …) + a task; each
 * provider runs server-side on the gateway in its own git worktree (no collisions).
 * Every dispatched lane is persisted backend-side (`lane_runs`) and followed
 * by a watcher that folds the run's events into the row, so the list here is
 * just a render of `list_lane_runs` + the `lanes:updated` event — runs survive
 * tab switches and app restarts instead of vanishing fire-and-forget.
 */
export function MultiProviderPane() {
  const selected = useCortexStore((s) => s.selectedProviders);
  const setSelected = useCortexStore((s) => s.setSelectedProviders);

  const [providers, setProviders] = useState<string[]>([]);
  const [project, setProject] = useState<string>(() => {
    try {
      return localStorage.getItem("cortex.lanes.project") || "";
    } catch {
      return "";
    }
  });
  const [task, setTask] = useState("");
  const [lanes, setLanes] = useState<LaneRunRecord[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [providersLoading, setProvidersLoading] = useState(true);
  const [laneError, setLaneError] = useState<string | null>(null);
  // In-app review: which lane is being reviewed, the fetched PR/diff, and the
  // merge lifecycle. `reviewingId` is set while the PR/diff loads so the card
  // button can show progress; `review` opens the modal.
  const [reviewingId, setReviewingId] = useState<string | null>(null);
  const [review, setReview] = useState<LaneReview | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [merging, setMerging] = useState(false);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const rows = await listLaneRuns();
      if (mounted.current) setLanes(rows);
    } catch {
      /* table renders from the last good list */
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    listProviders()
      .then((p) => {
        if (mounted.current) setProviders(p);
      })
      .catch((e) => {
        if (mounted.current) setError(humanizeError(e));
      })
      .finally(() => {
        if (mounted.current) setProvidersLoading(false);
      });
    void refresh();
    const unlisten = listen("lanes:updated", () => void refresh());
    return () => {
      mounted.current = false;
      void unlisten.then((u) => u());
    };
  }, [refresh]);

  function toggle(p: string) {
    setSelected(selected.includes(p) ? selected.filter((x) => x !== p) : [...selected, p]);
  }

  function parseProject(): { owner: string; repo: string } | null {
    const m = project.trim().match(/^([\w.-]+)\/([\w.-]+?)(?:\.git)?$/);
    return m ? { owner: m[1], repo: m[2] } : null;
  }

  async function run() {
    setError(null);
    const proj = parseProject();
    if (!proj) {
      setError("Enter the project as owner/repo (e.g. NetworkBound/cortex).");
      return;
    }
    if (selected.length < 2) {
      setError("Select at least 2 providers to run in parallel.");
      return;
    }
    if (!task.trim()) {
      setError("Describe the task for the providers to work on.");
      return;
    }
    try {
      localStorage.setItem("cortex.lanes.project", project.trim());
    } catch {
      /* ignore */
    }
    setBusy(true);
    try {
      await runProviderLanes(proj.owner, proj.repo, selected, task.trim());
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function stop(runId: string) {
    setLaneError(null);
    try {
      await stopLaneRun(runId);
    } catch (e) {
      setLaneError(humanizeError(e));
    }
    await refresh();
  }

  async function remove(runId: string) {
    setLaneError(null);
    try {
      await deleteLaneRun(runId);
    } catch (e) {
      setLaneError(humanizeError(e));
    }
    await refresh();
  }

  async function reattach(runId: string) {
    setLaneError(null);
    try {
      await reattachLaneRun(runId);
    } catch (e) {
      setLaneError(humanizeError(e));
    }
    // Outcome (running / still-interrupted detail) arrives via lanes:updated.
    await refresh();
  }

  async function openReview(runId: string) {
    setLaneError(null);
    setReviewError(null);
    setReviewingId(runId);
    try {
      const r = await laneReview(runId);
      if (mounted.current) setReview(r);
    } catch (e) {
      if (mounted.current) setLaneError(humanizeError(e));
    } finally {
      if (mounted.current) setReviewingId(null);
    }
  }

  async function mergeWinner() {
    if (!review) return;
    setReviewError(null);
    setMerging(true);
    try {
      await mergeLaneRun(review.run_id, review.pr_number);
      if (mounted.current) {
        setReview({ ...review, merged: true, state: "closed" });
      }
      await refresh();
    } catch (e) {
      if (mounted.current) setReviewError(humanizeError(e));
    } finally {
      if (mounted.current) setMerging(false);
    }
  }

  function closeReview() {
    setReview(null);
    setReviewError(null);
  }

  return (
    <div className="lanes-pane">
      <div>
        <h2 className="lanes-title">Parallel providers</h2>
        <p className="lanes-subtitle">
          Run several providers on one project at once — each in its own git
          worktree on the gateway, so their edits never collide.
        </p>
      </div>

      <label className="lanes-label">
        Project (Gitea <code>owner/repo</code>)
        <input
          className="lanes-input"
          value={project}
          onChange={(e) => setProject(e.target.value)}
          placeholder="NetworkBound/cortex"
        />
      </label>

      <div>
        <div className="lanes-label lanes-label-row">
          Providers {selected.length > 0 ? `(${selected.length} selected)` : ""}
        </div>
        <div className="lanes-chip-row">
          {providersLoading && providers.length === 0 && (
            <span className="lanes-hint">Loading providers…</span>
          )}
          {!providersLoading && providers.length === 0 && (
            <span className="lanes-hint">
              No providers available — connect a Cortex Gateway to run parallel lanes.
            </span>
          )}
          {providers.map((p) => (
            <button
              key={p}
              onClick={() => toggle(p)}
              className={selected.includes(p) ? "lanes-chip lanes-chip-on" : "lanes-chip"}
            >
              {p}
            </button>
          ))}
        </div>
      </div>

      <label className="lanes-label">
        Task
        <textarea
          className="lanes-input lanes-textarea"
          value={task}
          onChange={(e) => setTask(e.target.value)}
          placeholder="Describe what each provider should do on the project…"
          rows={4}
        />
      </label>

      <div className="lanes-actions">
        <button onClick={() => void run()} disabled={busy} className="btn-primary">
          {busy ? "Launching lanes…" : `Run ${selected.length || ""} lanes`}
        </button>
        {error && <span className="lanes-error">{error}</span>}
      </div>

      <div className="lanes-list">
        <div className="lanes-label lanes-label-row">Lanes</div>
        {laneError && <div className="lanes-error">{laneError}</div>}
        {lanes.length === 0 && (
          <p className="lanes-hint">
            No lane runs yet. Pick a project and a couple of providers above —
            each run lands here and stays put across tabs and restarts.
          </p>
        )}
        {lanes.map((l) => (
          <div key={l.run_id} className="lanes-card">
            <div className="lanes-card-head">
              <strong className="lanes-card-provider">{l.provider}</strong>
              <span className="lanes-card-meta">
                {l.owner}/{l.repo} · {timeAgo(l.updated_at)}
              </span>
              {l.merged_at != null && (
                <span className="status-pill lanes-status-merged">merged</span>
              )}
              <span className={`status-pill lanes-status-${l.status}`}>{l.status}</span>
            </div>
            <div className="lanes-card-task">{l.task}</div>
            {l.branch && <code className="lanes-card-branch">branch {l.branch}</code>}
            {l.detail && (
              <div className={l.status === "error" ? "lanes-card-detail lanes-error" : "lanes-card-detail"}>
                {l.detail}
              </div>
            )}
            <div className="lanes-card-actions">
              {l.status === "interrupted" && (
                <button
                  className="btn-ghost lanes-card-btn"
                  onClick={() => void reattach(l.run_id)}
                  title="Re-follow the run's event stream — picks it back up if it's still live on the gateway"
                >
                  Reattach
                </button>
              )}
              {l.status !== "running" && l.branch && (
                <button
                  className="btn-ghost lanes-card-btn"
                  onClick={() => void openReview(l.run_id)}
                  disabled={reviewingId === l.run_id}
                  title="Open the lane's diff against the project and merge it from here"
                >
                  {reviewingId === l.run_id ? "Opening…" : l.merged_at != null ? "View merge" : "Review"}
                </button>
              )}
              {l.status === "running" ? (
                <button className="btn-ghost lanes-card-btn" onClick={() => void stop(l.run_id)}>
                  Stop
                </button>
              ) : (
                <button className="btn-ghost lanes-card-btn" onClick={() => void remove(l.run_id)}>
                  Remove
                </button>
              )}
            </div>
          </div>
        ))}
        {lanes.length > 0 && (
          <p className="lanes-hint">
            Each lane edits the project in its own worktree on the gateway
            (branch <code>cortex/&lt;run&gt;/&lt;provider&gt;</code>). When a lane
            settles, hit <em>Review</em> to see its diff and merge the winner
            without leaving Cortex.
          </p>
        )}
      </div>

      {review && (
        <div className="modal-backdrop" onClick={closeReview}>
          <div className="modal lanes-review-modal" onClick={(e) => e.stopPropagation()}>
            <h2>{review.title}</h2>
            <div className="lanes-review-meta">
              <code className="lanes-card-branch">
                {review.branch} → {review.base}
              </code>
              <a
                className="lanes-review-pr-link"
                href={review.pr_url}
                target="_blank"
                rel="noreferrer"
              >
                PR #{review.pr_number} on Gitea ↗
              </a>
            </div>
            {review.merged ? (
              <div className="lanes-review-note lanes-review-note-ok">
                This lane is merged into <code>{review.base}</code>.
              </div>
            ) : !review.mergeable ? (
              <div className="lanes-review-note lanes-review-note-warn">
                Gitea can't merge this branch automatically — it conflicts with{" "}
                <code>{review.base}</code>. Resolve the conflicts on Gitea, then
                review again.
              </div>
            ) : null}
            {review.diff.trim() ? (
              <pre className="lanes-diff">{review.diff}</pre>
            ) : (
              <p className="lanes-hint">
                The diff is empty — the lane's branch has no changes beyond{" "}
                <code>{review.base}</code>.
              </p>
            )}
            {review.diff_truncated && (
              <p className="lanes-hint">
                Large change — the diff shown here is truncated. The PR on Gitea
                has the full picture.
              </p>
            )}
            {reviewError && <div className="lanes-error">{reviewError}</div>}
            <div className="lanes-review-actions">
              <button className="btn-ghost" onClick={closeReview}>
                Close
              </button>
              <button
                className="btn-primary"
                onClick={() => void mergeWinner()}
                disabled={merging || review.merged || !review.mergeable}
              >
                {review.merged ? "Merged ✓" : merging ? "Merging…" : "Merge winner"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
