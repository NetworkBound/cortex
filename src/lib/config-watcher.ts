import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Payload emitted on the `config-changed` window event. Mirrors
 * `ConfigChangedEvent` in `src-tauri/src/commands/config_watcher.rs`.
 *
 * Future consumers (snippets panel, trust matrix editor, theme picker, etc.)
 * can subscribe via `subscribeConfigChanges` and refresh their cached state
 * whenever a relevant config file is touched on disk. This module ships the
 * event stream only — no consumers are wired yet.
 */
export interface ConfigChangedEvent {
  /** One of "created", "modified", "deleted". */
  kind: "created" | "modified" | "deleted";
  /** Absolute path of the changed config file. */
  path: string;
  /** Unix epoch milliseconds. */
  ts: number;
}

/** Status snapshot returned by `config_watcher_status`. */
export interface ConfigWatcherStatus {
  active: boolean;
  watched_paths: string[];
}

/**
 * Stop the global config watcher. Resolves to `true` if a watcher was
 * actually stopped. Mostly useful for tests / dev tools — the watcher is
 * auto-started by the Tauri setup hook and normally runs for the lifetime
 * of the app.
 */
export async function stopConfigWatcher(): Promise<boolean> {
  return invoke<boolean>("stop_config_watcher");
}

/** Snapshot of the watcher's current state. */
export async function configWatcherStatus(): Promise<ConfigWatcherStatus> {
  return invoke<ConfigWatcherStatus>("config_watcher_status");
}

/**
 * Subscribe to `config-changed` events. Returns an unlisten function — call
 * it on unmount to detach the listener.
 *
 * @example
 *   useEffect(() => {
 *     let off: UnlistenFn | undefined;
 *     subscribeConfigChanges((evt) => {
 *       if (evt.path.endsWith("snippets.json")) reload();
 *     }).then((fn) => { off = fn; });
 *     return () => { off?.(); };
 *   }, []);
 */
export async function subscribeConfigChanges(
  cb: (event: ConfigChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConfigChangedEvent>("config-changed", (evt) => cb(evt.payload));
}
