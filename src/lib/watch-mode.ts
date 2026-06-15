import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Payload emitted by the Rust watcher on each detected `AI!` marker.
 * Mirrors the `WatchTriggerPayload` struct in
 * `src-tauri/src/watch_mode.rs`.
 */
export interface WatchTriggerPayload {
  /** Absolute path to the file that triggered. */
  path: string;
  /** 1-indexed line number where the marker was found. */
  line: number;
  /** Comment style — `"//"` for slash-style, `"#"` for hash-style. */
  marker: string;
  /** Up-to-9-line excerpt centered on the matched line. */
  context: string;
  /** Unix epoch milliseconds when the event was emitted by the backend. */
  ts: number;
}

/**
 * Start (or restart) watch mode against the supplied absolute directory
 * roots.
 *
 * NOTE: the current Rust backend (`commands::watch_mode::start_watch_mode`)
 * accepts a single `project_root` and replaces any existing watcher on each
 * call. This wrapper accepts `roots: string[]` for forward-compatibility
 * with a future multi-root backend; today it forwards the first non-empty
 * root and ignores the rest (logging a warning in dev builds).
 */
export async function startWatchMode(roots: string[]): Promise<void> {
  const cleaned = roots.map((r) => r.trim()).filter((r) => r.length > 0);
  if (cleaned.length === 0) {
    throw new Error("startWatchMode: at least one root path is required");
  }
  if (cleaned.length > 1 && typeof console !== "undefined") {
    console.warn(
      `watch-mode: backend currently supports a single root; using ${cleaned[0]} and ignoring ${cleaned.length - 1} additional root(s).`,
    );
  }
  await invoke<void>("start_watch_mode", { projectRoot: cleaned[0] });
}

/** Stop watch mode if running. Resolves to `true` if a watcher was stopped. */
export async function stopWatchMode(): Promise<boolean> {
  return invoke<boolean>("stop_watch_mode");
}

/** Returns `true` if a watcher is currently running. */
export async function isWatchModeActive(): Promise<boolean> {
  return invoke<boolean>("is_watch_mode_active");
}

/**
 * Subscribe to `watch-mode-trigger` events from the backend. The returned
 * function unsubscribes.
 */
export async function subscribeWatchTriggers(
  cb: (payload: WatchTriggerPayload) => void,
): Promise<UnlistenFn> {
  return listen<WatchTriggerPayload>("watch-mode-trigger", (evt) => cb(evt.payload));
}
