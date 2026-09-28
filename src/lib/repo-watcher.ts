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
