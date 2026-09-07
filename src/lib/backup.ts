import { invoke } from "@tauri-apps/api/core";

/**
 * Full Cortex backup + restore client. Mirrors `BackupMeta` / `RestoreReport`
 * in `src-tauri/src/commands/backup.rs`. Tarballs live at
 * `~/.cortex/backups/<unix_ms>-<label>.tar.gz`.
 *
 * Restore is exposed in two phases — the panel always calls `dry_run: true`
 * first to surface a confirmation summary before letting the user commit.
 */

export interface BackupMeta {
  id: string;
  label: string;
  created_unix_ms: number;
  size_bytes: number;
  file_count: number;
  roots: string[];
}

export interface RestoreReport {
  files_restored: number;
  files_skipped: number;
  errors: string[];
}

export async function createBackup(label: string): Promise<BackupMeta> {
  return invoke<BackupMeta>("create_backup", { label });
}

export async function listBackups(): Promise<BackupMeta[]> {
  return invoke<BackupMeta[]>("list_backups");
}

export async function restoreBackup(
  id: string,
  dryRun: boolean,
): Promise<RestoreReport> {
  // Tauri snake-case the named arg — the command signature is `dry_run`.
  return invoke<RestoreReport>("restore_backup", { id, dryRun });
}

export async function deleteBackup(id: string): Promise<void> {
  return invoke("delete_backup", { id });
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export { timeAgo } from "@/lib/time";
