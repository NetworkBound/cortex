import { invoke } from "@tauri-apps/api/core";
import { timeAgo as relativeTime } from "@/lib/time";

/**
 * Client surface for the Gitea backup auto-mirror. Mirrors the Rust types in
 * `src-tauri/src/commands/gitea_backup.rs`. The backup loop runs every 6
 * hours in the Tauri runtime once the user enables it — these functions
 * are how the UI configures + triggers it.
 *
 * Workflow:
 *   1. UI loads `getSettings()` to populate the form.
 *   2. User edits + clicks Save → `setSettings({ ..., enabled: true })`.
 *   3. User clicks Backup now (or `/backup-now`) → `runBackupNow()`.
 *   4. The scheduler in `lib.rs` picks up the next 6h tick and uses the
 *      same settings without a restart.
 */

export interface GiteaConfig {
  base_url: string;
  token: string;
  owner: string;
  repo: string;
}

export interface BackupReport {
  repo_url: string;
  commits_made: number;
  files_added: number;
  files_changed: number;
  files_deleted: number;
  bytes_total: number;
  errors: string[];
  dry_run: boolean;
  started_unix_ms: number;
  finished_unix_ms: number;
}

export interface GiteaSettings {
  enabled: boolean;
  base_url: string;
  token: string;
  owner: string;
  repo: string;
  last_backup_unix_ms: number;
  last_report: BackupReport | null;
}

const DEFAULT_SETTINGS: GiteaSettings = {
  enabled: false,
  base_url: "",
  token: "",
  owner: "",
  repo: "",
  last_backup_unix_ms: 0,
  last_report: null,
};

/** Load the persisted settings. Missing config → defaults (not an error). */
export async function getSettings(): Promise<GiteaSettings> {
  try {
    const s = await invoke<GiteaSettings | null>("gitea_get_settings");
    return s ?? DEFAULT_SETTINGS;
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export async function setSettings(settings: GiteaSettings): Promise<void> {
  await invoke("gitea_set_settings", { settings });
}

/**
 * Run an explicit backup. `dry_run: true` walks + counts without writing
 * to the mirror or pushing — used by a future "Preview" button.
 */
export async function runBackup(
  config: GiteaConfig,
  dryRun: boolean,
): Promise<BackupReport> {
  return invoke<BackupReport>("gitea_backup", { config, dryRun });
}

/**
 * Trigger the same backup the scheduler runs, using whatever is currently
 * saved on disk. Errors out if the user hasn't enabled / configured it yet.
 */
export async function runBackupNow(): Promise<BackupReport> {
  return invoke<BackupReport>("gitea_backup_now");
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** "never" when no backup has run yet, otherwise relative. */
export function timeAgo(ts: number): string {
  return relativeTime(ts, { empty: "never" });
}
