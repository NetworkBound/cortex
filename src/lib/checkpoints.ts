import { invoke } from "@tauri-apps/api/core";

export interface CheckpointInfo {
  id: string;
  ts: number;
  label: string | null;
  size_bytes: number;
  file_count: number;
}

export async function createCheckpoint(
  projectRoot: string,
  label?: string,
): Promise<CheckpointInfo> {
  return invoke<CheckpointInfo>("create_checkpoint", {
    projectRoot,
    label: label ?? null,
  });
}

export async function listCheckpoints(
  projectRoot: string,
): Promise<CheckpointInfo[]> {
  return invoke<CheckpointInfo[]>("list_checkpoints", { projectRoot });
}

export async function restoreCheckpoint(
  projectRoot: string,
  id: string,
  force = false,
): Promise<void> {
  return invoke("restore_checkpoint", { projectRoot, id, force });
}

/**
 * Restore the most-recent checkpoint — the quick "undo" path (aider's `/undo`),
 * complementing the snapshot auto-taken before `/apply`. Returns the restored
 * checkpoint's metadata, or `null` when the project has no checkpoints to undo.
 * Defaults to `force` because undo's purpose is to roll the working tree back to
 * the snapshot even when it has uncommitted changes.
 */
export async function restoreLastCheckpoint(
  projectRoot: string,
  force = true,
): Promise<CheckpointInfo | null> {
  return invoke<CheckpointInfo | null>("restore_last_checkpoint", {
    projectRoot,
    force,
  });
}

/** What restoring this checkpoint would do to a single file. */
export type CheckpointDiffStatus = "added" | "modified" | "removed";

export interface CheckpointDiffEntry {
  path: string;
  status: CheckpointDiffStatus;
  /** Worktree (current) contents — null for `added`, binary, or oversize. */
  old_content: string | null;
  /** Checkpoint contents — null for `removed`, binary, or oversize. */
  new_content: string | null;
  /** Either side binary/oversize: render a placeholder, not a line diff. */
  binary: boolean;
  old_size: number;
  new_size: number;
}

export interface CheckpointDiff {
  id: string;
  added: number;
  modified: number;
  removed: number;
  entries: CheckpointDiffEntry[];
}

/**
 * Compute what restoring `id` would change against the live worktree, without
 * mutating anything. Backed by the read-only `diff_checkpoint` command.
 */
export async function diffCheckpoint(
  projectRoot: string,
  id: string,
): Promise<CheckpointDiff> {
  return invoke<CheckpointDiff>("diff_checkpoint", { projectRoot, id });
}

export async function deleteCheckpoint(
  projectRoot: string,
  id: string,
): Promise<void> {
  return invoke("delete_checkpoint", { projectRoot, id });
}

export async function pruneCheckpoints(projectRoot: string): Promise<number> {
  return invoke<number>("prune_checkpoints", { projectRoot });
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

export { timeAgo } from "@/lib/time";
