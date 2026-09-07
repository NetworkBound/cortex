import { invoke } from "@tauri-apps/api/core";

/**
 * Memory snapshot metadata as returned by the Rust backend. Mirrors
 * `SnapshotMeta` in `src-tauri/src/memory/snapshots.rs`.
 */
export interface SnapshotMeta {
  id: string;
  label: string;
  created_unix_ms: number;
  size_bytes: number;
  file_count: number;
  roots: string[];
}

/** Result of restoring a snapshot. */
export interface RollbackReport {
  files_restored: number;
  files_skipped: number;
  errors: string[];
}

export async function createSnapshot(
  label: string,
  activeProject?: string | null,
): Promise<SnapshotMeta> {
  return invoke<SnapshotMeta>("create_snapshot", {
    label,
    activeProject: activeProject ?? null,
  });
}

export async function listSnapshots(): Promise<SnapshotMeta[]> {
  return invoke<SnapshotMeta[]>("list_snapshots");
}

export async function rollbackSnapshot(id: string): Promise<RollbackReport> {
  return invoke<RollbackReport>("rollback_snapshot", { id });
}

export async function deleteSnapshot(id: string): Promise<void> {
  return invoke("delete_snapshot", { id });
}

export async function pruneSnapshots(keep: number): Promise<number> {
  return invoke<number>("prune_snapshots", { keep });
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

export { timeAgo } from "@/lib/time";
