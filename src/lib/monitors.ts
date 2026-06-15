import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * One row from `<project_root>/.cortex/monitors/monitors.json`. Mirrors the
 * Rust `MonitorSpec` struct in `src-tauri/src/monitors.rs`.
 */
export interface MonitorSpec {
  name: string;
  command: string;
  args: string[];
  level: "info" | "warn" | "error";
}

/**
 * Payload emitted on each line of monitor output. Mirrors the Rust
 * `MonitorLinePayload` struct in `src-tauri/src/monitors.rs`.
 */
export interface MonitorLinePayload {
  /** Monitor name, as configured in monitors.json. */
  name: string;
  /** One line of stdout or stderr from the child process. */
  line: string;
  /** Severity tag; stderr lines are bumped one notch from the spec default. */
  level: "info" | "warn" | "error";
  /** Unix epoch milliseconds when the line was forwarded. */
  ts: number;
}

/**
 * Start every monitor in `<project_root>/.cortex/monitors/monitors.json`.
 * Stops any previously running monitors first. Returns the names of the
 * monitors that were successfully spawned.
 */
export async function startMonitors(projectRoot: string): Promise<string[]> {
  return invoke<string[]>("start_monitors", { projectRoot });
}

/** Stop every running monitor. Idempotent. */
export async function stopMonitors(): Promise<void> {
  await invoke<void>("stop_monitors");
}

/**
 * Read `<project_root>/.cortex/monitors/monitors.json` without starting
 * anything. A missing file resolves to an empty list.
 */
export async function listMonitors(projectRoot: string): Promise<MonitorSpec[]> {
  return invoke<MonitorSpec[]>("list_monitors", { projectRoot });
}

/**
 * Subscribe to `monitor-line` events from the backend. The returned function
 * unsubscribes.
 */
export async function subscribeMonitorLines(
  cb: (payload: MonitorLinePayload) => void,
): Promise<UnlistenFn> {
  return listen<MonitorLinePayload>("monitor-line", (evt) => cb(evt.payload));
}
