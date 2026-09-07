import { invoke } from "@tauri-apps/api/core";

/**
 * Fire an OS-level desktop notification via the Tauri backend.
 *
 * Failures are surfaced so the caller can fall back to an in-app toast
 * when no notification daemon is available (e.g. headless Linux). Title
 * is required and silently truncated at 256 chars; body at 1024.
 */
export async function desktopNotify(title: string, body: string = ""): Promise<void> {
  return invoke("desktop_notify", { args: { title, body } });
}
