/**
 * Thin TS wrapper around the `save_file_text` Tauri command.
 *
 * Lives separately from `editor.ts` so the open-side helpers don't get
 * dragged into anything that just wants to persist a buffer. The backend
 * validates the path + body — this side just forwards.
 */
import { invoke } from "@tauri-apps/api/core";

/**
 * Persist `body` to `path` on disk. Returns the absolute path that was
 * written (mostly useful for logging — it matches `path` after backend
 * normalisation).
 *
 * Throws if the backend rejects the save (empty path, oversized body,
 * I/O error). Callers should surface the error message to the user.
 */
export async function saveFileText(path: string, body: string): Promise<string> {
  return await invoke<string>("save_file_text", { path, body });
}
