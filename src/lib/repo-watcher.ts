import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Payload emitted on the `repo-watcher:event` window event. Mirrors
 * `RepoWatcherEvent` in `src-tauri/src/repo_map.rs::watcher`.
 */
export interface RepoWatcherEvent {
  /** One of "modified", "created", "deleted". */
  kind: "modified" | "created" | "deleted";
  /** Absolute path of the changed entry. */
  path: string;
  /** Project root the watcher was started against. */
  project_root: string;
  /** Unix epoch milliseconds. */
  ts: number;
}

/** Status snapshot returned by `repo_watcher_status`. */
export interface RepoWatcherStatus {
  active_projects: string[];
  last_change_ms: number | null;
  change_count_since_index: number;
}

/**
 * Start (or restart) the repo watcher for `projectRoot`. Safe to call
 * repeatedly when the active project changes — the backend replaces any
 * existing watcher for the same root.
 */
export async function startRepoWatcher(projectRoot: string): Promise<void> {
  await invoke<void>("start_repo_watcher", { projectRoot });
}

/** Stop the watcher for `projectRoot`. Resolves to `true` if a watcher was stopped. */
export async function stopRepoWatcher(projectRoot: string): Promise<boolean> {
  return invoke<boolean>("stop_repo_watcher", { projectRoot });
}

/** Snapshot of active watchers and aggregate change stats. */
export async function repoWatcherStatus(): Promise<RepoWatcherStatus> {
  return invoke<RepoWatcherStatus>("repo_watcher_status");
}

/** Reset the change counter for `projectRoot` after a successful re-index. */
export async function repoWatcherReset(projectRoot: string): Promise<void> {
  await invoke<void>("repo_watcher_reset", { projectRoot });
}

/**
 * Subscribe to `repo-watcher:event` events. Returns an unlisten function.
 */
export async function subscribeRepoWatcher(
  cb: (event: RepoWatcherEvent) => void,
): Promise<UnlistenFn> {
  return listen<RepoWatcherEvent>("repo-watcher:event", (evt) =>
    cb(evt.payload),
  );
}
