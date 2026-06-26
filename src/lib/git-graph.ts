import type { Commit } from "./git";

/**
 * DAG lane assignment for the git history panel.
 *
 * Commits arrive newest-first from `git log`. We walk them in that order and
 * assign each one a *lane* (column index) so the SVG mini-graph can draw a
 * column-per-branch view, SourceTree style.
 *
 * Algorithm:
 *   1. For each commit (top-down):
 *      - If a previous commit reserved a lane for our hash (because we're its
 *        parent), use that lane.
 *      - Otherwise, allocate the lowest-index free lane.
 *   2. The first parent inherits our lane (mainline of a branch flows
 *      straight down). Additional parents (merges) reserve fresh lanes.
 *   3. When a lane is no longer pointing at any upcoming hash, it's freed and
 *      may be reused below — keeps the graph narrow on long-lived repos.
 *
 * Output is positional: `lanes[i]` corresponds to `commits[i]`.
 */

/** 8-color palette cycled per lane. Matches the brief. */
export const LANE_COLORS = [
  "#f59e0b", // amber
  "#3b82f6", // blue
  "#22c55e", // green
  "#a855f7", // purple
  "#ef4444", // red
  "#06b6d4", // cyan
  "#ec4899", // pink
  "#eab308", // yellow
] as const;

export interface LanedCommit {
  /** Lane this commit's node sits in (0-indexed). */
  lane: number;
  /** Lanes its parents will occupy in the next row(s). Order matches `commit.parents`. */
  parentLanes: number[];
  /** Resolved color for the commit's own lane. */
  color: string;
  /** Max lane index in use at this row — useful for sizing the SVG. */
  width: number;
}

export function laneColor(lane: number): string {
  return LANE_COLORS[lane % LANE_COLORS.length];
}

export function assignLanes(commits: Commit[]): LanedCommit[] {
  // `lanes[i]` = the hash we expect to occupy lane `i` next, or null if free.
  const lanes: Array<string | null> = [];
  const result: LanedCommit[] = [];

  const pickFree = (): number => {
    for (let i = 0; i < lanes.length; i++) {
      if (lanes[i] === null) return i;
    }
    lanes.push(null);
    return lanes.length - 1;
  };

  for (const commit of commits) {
    // 1. Find our lane: either one previously reserved, or a fresh one.
    let lane = lanes.findIndex((h) => h === commit.hash);
    if (lane === -1) {
      lane = pickFree();
    }

    // 2. Clear any *other* lanes that were also waiting on this hash. (Happens
    // when multiple children share a single parent — they all converge here.)
    for (let i = 0; i < lanes.length; i++) {
      if (i !== lane && lanes[i] === commit.hash) {
        lanes[i] = null;
      }
    }

    // 3. Assign parent lanes. First parent inherits our lane (mainline);
    // additional parents (merge) claim fresh lanes — unless another lane is
    // already reserving that parent, in which case we reuse it.
    const parentLanes: number[] = [];
    commit.parents.forEach((parent, idx) => {
      if (idx === 0) {
        lanes[lane] = parent;
        parentLanes.push(lane);
      } else {
        // If a different lane is already waiting on this parent, share it.
        const existing = lanes.findIndex((h) => h === parent);
        if (existing !== -1) {
          parentLanes.push(existing);
        } else {
          const newLane = pickFree();
          lanes[newLane] = parent;
          parentLanes.push(newLane);
        }
      }
    });

    // 4. Root commit (no parents) frees its lane.
    if (commit.parents.length === 0) {
      lanes[lane] = null;
    }

    // 5. Compact trailing nulls so `width` doesn't drift upward forever.
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) {
      lanes.pop();
    }

    result.push({
      lane,
      parentLanes,
      color: laneColor(lane),
      width: Math.max(lane, ...parentLanes, lanes.length - 1) + 1,
    });
  }

  return result;
}
