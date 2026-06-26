import { invoke } from "@tauri-apps/api/core";

/**
 * Wire types mirroring `orchestrator::teams::{Team, Worker}`. A "team" is a
 * named coordination unit persisted at `~/.cortex/teams/<id>.json` and
 * surfaced in the orchestrator dashboard.
 *
 * Status pills render off `WorkerStatus`; any string outside the union is
 * rejected at write-time on the Rust side so the renderer can be exhaustive.
 */

export type WorkerStatus = "idle" | "working" | "blocked" | "done" | "error";

export const WORKER_STATUSES: WorkerStatus[] = [
  "idle",
  "working",
  "blocked",
  "done",
  "error",
];

/** Team-level run lifecycle (see `Team.run_status`). */
export type TeamRunStatus = "planning" | "running" | "done" | "error";

export interface Worker {
  role: string;
  agent_id: string;
  current_task?: string | null;
  status: WorkerStatus | string;
  started_unix_ms: number;
  last_event_unix_ms: number;
  message_count: number;
  /** Chat session holding this worker's latest run transcript. */
  session_id?: string | null;
  /**
   * Subtask classification from the manager's plan (orchestration slice 2):
   * `"chat" | "code"` and `"easy" | "medium" | "hard"`. Absent until a run tags
   * the worker; cost-aware dispatch (slice 3) reads them.
   */
  task_kind?: string | null;
  task_difficulty?: string | null;
  /**
   * Cost-aware dispatch record (slice 3). `effective_model` is the concrete
   * model this worker ran on (a role pin or the cost router's pick);
   * `projected_usd` is the estimated spend for its latest run (0 for free local
   * models). Absent until a run dispatches the worker.
   */
  effective_model?: string | null;
  projected_usd?: number | null;
  /**
   * The `lane_runs` id a `code`-tagged worker's subtask was dispatched to
   * (orchestration slice 4): with a repo bound, code work edits it in a gateway
   * worktree lane instead of producing chat. Absent for chat workers. Deep-links
   * the run into the Lanes tab.
   */
  lane_run_id?: string | null;
}

export interface Team {
  id: string;
  name: string;
  manager_role: string;
  workers: Worker[];
  created_unix_ms: number;
  /** The goal handed to the manager on the most recent run. */
  goal?: string | null;
  /** Lifecycle of the most recent run; absent = never run. */
  run_status?: TeamRunStatus | string | null;
  last_run_unix_ms?: number | null;
  /** Chat session holding the manager's planning transcript. */
  plan_session_id?: string | null;
  /**
   * Total estimated USD across the most recent run's workers (slice 3). Absent
   * until a run completes; `0` is legitimate (an all-local run is free).
   */
  spent_usd?: number | null;
  /**
   * Chat session holding the manager's synthesis + verification pass over all
   * the workers' outputs (slice 5). Absent for single-worker / trivially-thin
   * runs (the synthesis gate) and until a multi-worker run completes.
   */
  synthesis_session_id?: string | null;
  /**
   * Optional soft spend ceiling in USD (slice 6). When set, the dashboard
   * compares the run's projected `spent_usd` against it and paints an
   * over-budget warning if exceeded — it never blocks or kills a run. Absent =
   * no budget. A team-level setting (survives runs, unlike `spent_usd`).
   */
  budget_usd?: number | null;
}

/** True while the team's most recent run is still in flight. */
export function isTeamRunning(team: Team): boolean {
  return team.run_status === "planning" || team.run_status === "running";
}

/** List every team under `~/.cortex/teams/*.json`, newest first. */
export async function listTeams(): Promise<Team[]> {
  return invoke<Team[]>("list_teams");
}

/** Load a single team by id. Throws on missing / malformed. */
export async function getTeam(id: string): Promise<Team> {
  return invoke<Team>("get_team", { id });
}

/**
 * Create + persist a new team. Workers all start in `idle`. `budgetUsd` is an
 * optional soft spend ceiling (slice 6); `null` = no budget.
 */
export async function createTeam(
  name: string,
  managerRole: string,
  workerRoles: string[],
  budgetUsd: number | null = null,
): Promise<Team> {
  return invoke<Team>("create_team", {
    name,
    managerRole,
    workerRoles,
    budgetUsd,
  });
}

/**
 * Mutate one worker. `status` must be a `WorkerStatus`. `currentTask = null`
 * leaves the existing task untouched; pass a string (including `""`) to set.
 */
export async function updateTeamWorker(
  teamId: string,
  workerId: string,
  status: WorkerStatus,
  currentTask: string | null = null,
): Promise<Team> {
  return invoke<Team>("update_team_worker", {
    teamId,
    workerId,
    status,
    currentTask,
  });
}

/**
 * Kick off a real team run: the manager plans one task per worker, then the
 * workers execute concurrently through the adapter registry. Resolves as soon
 * as the run is accepted (team comes back stamped `planning`); progress lands
 * in the team file — follow it via polling or the `teams:updated` event.
 */
export async function runTeam(
  teamId: string,
  goal: string,
  model: string | null = null,
  repo: string | null = null,
  budgetUsd: number | null = null,
): Promise<Team> {
  return invoke<Team>("run_team", { teamId, goal, model, repo, budgetUsd });
}

/** Remove a team file. Missing files are a no-op. */
export async function deleteTeam(id: string): Promise<void> {
  return invoke("delete_team", { id });
}

/** Human-friendly "5m ago" rendering for `*_unix_ms` columns. */
export { timeAgo } from "@/lib/time";

/** Short truncate for the per-card task line — keeps cards uniform height. */
export function truncateTask(task: string | null | undefined, max = 90): string {
  if (!task) return "—";
  const t = task.trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max - 1)}…`;
}

/**
 * Format an estimated USD spend for the cost-aware dispatch surface (slice 3).
 * Free (all-local) runs read "Free" rather than "$0.00"; sub-cent amounts keep
 * enough precision to stay non-zero so a real-but-tiny estimate isn't hidden.
 */
export function formatUsd(usd: number | null | undefined): string {
  if (usd == null) return "—";
  if (usd <= 0) return "Free";
  if (usd < 0.01) return `<$0.01`;
  return `$${usd.toFixed(2)}`;
}

export interface BudgetStatus {
  /** The team's spend ceiling. */
  budget: number;
  /** The run's projected spend (0 when no run has completed yet). */
  projected: number;
  /** projected / budget clamped to [0, 1] for the gauge fill width. */
  ratio: number;
  /** True when the projection has crossed the budget (the soft-warn trigger). */
  over: boolean;
}

/**
 * Compare a team's projected spend against its soft budget (slice 6). Returns
 * `null` when no budget is set (nothing to gauge). A zero budget with any spend
 * reads as over; a zero budget with zero spend is exactly at-budget (not over),
 * so an all-free run under a $0 ceiling stays calm.
 */
export function budgetStatus(
  budget: number | null | undefined,
  spent: number | null | undefined,
): BudgetStatus | null {
  if (budget == null) return null;
  const projected = spent ?? 0;
  const ratio = budget > 0 ? Math.min(1, Math.max(0, projected / budget)) : projected > 0 ? 1 : 0;
  return { budget, projected, ratio, over: projected > budget };
}
