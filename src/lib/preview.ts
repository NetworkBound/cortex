import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Mirrors `crate::preview::DetectedServer` on the Rust side.
 * `title` is the parsed contents of the page's `<title>` tag when present.
 */
export interface DetectedServer {
  port: number;
  url: string;
  title: string | null;
}

/**
 * Run a one-shot port sweep right now. Used when the preview tab opens so
 * the UI doesn't have to wait up to 3s for the watcher's next tick.
 */
export async function listDevServers(): Promise<DetectedServer[]> {
  return invoke<DetectedServer[]>("list_dev_servers");
}

/**
 * Start the background dev-server poll loop. Safe to call repeatedly —
 * any existing watcher is replaced. The watcher itself is also kicked off
 * automatically from the Tauri `setup` hook, so most callers won't need
 * this.
 */
export async function startPreviewWatcher(): Promise<void> {
  await invoke<void>("start_preview_watcher");
}

/** Stop the background poll loop. Resolves to `true` if one was running. */
export async function stopPreviewWatcher(): Promise<boolean> {
  return invoke<boolean>("stop_preview_watcher");
}

/**
 * Subscribe to `preview:servers` events. The payload is the full live list
 * of detected dev servers — replace any local state on each event.
 * Returns an unlisten function.
 */
export async function subscribePreviewServers(
  cb: (servers: DetectedServer[]) => void,
): Promise<UnlistenFn> {
  return listen<DetectedServer[]>("preview:servers", (evt) => cb(evt.payload));
}
