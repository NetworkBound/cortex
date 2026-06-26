/**
 * Multi-provider runs — data layer (Phase A).
 *
 * Lets the user pick several providers/models (e.g. ChatGPT + Claude) to work a
 * task in parallel. This module owns the provider list + the worktree-per-
 * provider PLAN; the actual parallel dispatch + lane UI (Phase C) and the
 * gateway per-run `cwd` change (Phase B) build on top — see
 * docs/multi-provider-gateway-spec.md for the architecture and the open
 * project-access design decision that gates dispatch.
 */
import { invoke } from "@tauri-apps/api/core";
import { createWorktree, type Worktree } from "@/lib/worktrees";

/**
 * One persisted lane run — the row shape `lane_runs` serves (backend
 * `crate::lanes::LaneRunRecord`). Both the dispatch call and `list_lane_runs`
 * return this, so the pane renders one shape everywhere. The backend follows
 * each run's event stream and folds progress into `status`/`detail`, emitting
 * `lanes:updated` — re-list on that event to stay live.
 */
export interface LaneRunRecord {
  run_id: string;
  provider: string;
  owner: string;
  repo: string;
  task: string;
  /** gateway-side worktree branch; null when the lane failed to start. */
  branch: string | null;
  /** running | done | error | stopped | interrupted */
  status: string;
  /** Last humanized progress line (tool activity, status, error text). */
  detail: string | null;
  created_at: number;
  updated_at: number;
  /** Set when the lane branch was merged from the in-app review. */
  merged_at: number | null;
}

/** Everything the in-app review panel renders for one lane (backend `LaneReview`). */
export interface LaneReview {
  run_id: string;
  branch: string;
  base: string;
  pr_number: number;
  pr_url: string;
  /** `open` | `closed` (a merged PR is `closed` with `merged: true`). */
  state: string;
  merged: boolean;
  /** Gitea's conflict check — false means the merge needs manual resolution. */
  mergeable: boolean;
  title: string;
  /** Combined unified diff of the PR (capped backend-side with a note). */
  diff: string;
  diff_truncated: boolean;
}

/**
 * Launch one worktree-isolated gateway run per provider on the same task. The gateway
 * clones `<owner>/<repo>` from Gitea and runs each provider in its own git
 * worktree, so their edits never collide. Every lane (including ones that
 * failed to start) is persisted; returns the new records.
 */
export async function runProviderLanes(
  owner: string,
  repo: string,
  providers: string[],
  input: string,
  instructions?: string,
): Promise<LaneRunRecord[]> {
  return invoke<LaneRunRecord[]>("run_provider_lanes", {
    args: { owner, repo, providers, input, instructions: instructions ?? null },
  });
}

/** Persisted lane history, newest first (default limit 100). */
export async function listLaneRuns(limit?: number): Promise<LaneRunRecord[]> {
  return invoke<LaneRunRecord[]>("list_lane_runs", { limit: limit ?? null });
}

/** Stop a running lane (gateway stop + row stamped `stopped`). */
export async function stopLaneRun(runId: string): Promise<void> {
  await invoke("stop_lane_run", { runId });
}

/** Remove a settled lane from the history. Running lanes must be stopped first. */
export async function deleteLaneRun(runId: string): Promise<void> {
  await invoke("delete_lane_run", { runId });
}

/**
 * Re-follow an `interrupted` lane's event stream. Returns immediately; the row
 * flips back to `running` (then settles) via `lanes:updated` only if the run
 * is actually still live on the gateway — otherwise the status stays `interrupted`
 * with an honest detail line.
 */
export async function reattachLaneRun(runId: string): Promise<void> {
  await invoke("reattach_lane_run", { runId });
}

/**
 * Open (or adopt) the review PR for a settled lane on Gitea and fetch its
 * combined diff + merge state — what the "Review" panel renders.
 */
export async function laneReview(runId: string): Promise<LaneReview> {
  return invoke<LaneReview>("lane_review", { runId });
}

/** Merge the lane's review PR ("merge winner"); returns the updated lane row. */
export async function mergeLaneRun(runId: string, prNumber: number): Promise<LaneRunRecord> {
  return invoke<LaneRunRecord>("merge_lane_run", { runId, prNumber });
}

/** Fetch the provider/model ids the Cortex Gateway exposes (`/v1/models`). */
export async function listProviders(): Promise<string[]> {
  try {
    const ids = await invoke<string[]>("list_gateway_models");
    return Array.isArray(ids) ? ids : [];
  } catch {
    return [];
  }
}

/** A planned lane: one provider, its isolated worktree, and the branch. */
export interface ProviderLane {
  provider: string;
  worktree: Worktree;
}

/**
 * Provision one git worktree per provider so their edits never collide
 * ("ultimate speed, no conflicts"). Best-effort atomic: if any worktree fails
 * to create, the ones already created are rolled back so we don't leak
 * half-provisioned state. Returns the per-provider lane plan.
 *
 * NOTE: this provisions LOCAL worktrees (Cortex side). Whether the executing
 * agent (which runs server-side on the gateway) operates in these worktrees depends
 * on the project-access decision in the spec — until that lands, the lanes are
 * the isolation scaffold the dispatch step will target.
 */
export async function provisionProviderLanes(
  projectRoot: string,
  providers: string[],
): Promise<ProviderLane[]> {
  const lanes: ProviderLane[] = [];
  try {
    for (const provider of providers) {
      const worktree = await createWorktree(projectRoot, `parallel:${provider}`);
      lanes.push({ provider, worktree });
    }
    return lanes;
  } catch (e) {
    // Roll back anything we already created so a partial failure leaves no
    // orphaned worktrees.
    const { removeWorktree } = await import("@/lib/worktrees");
    for (const lane of lanes) {
      try {
        await removeWorktree(lane.worktree.id, false);
      } catch {
        /* best-effort cleanup */
      }
    }
    throw e;
  }
}
