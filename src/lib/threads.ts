/**
 * Thin TS wrappers around the `threads` Tauri commands defined in
 * `src-tauri/src/commands/threads.rs`. The Rust side persists each thread as
 * one JSON document under `<project_root>/.cortex/threads/<id>.json`; we
 * surface a flat API the frontend can call without thinking about IPC
 * argument casing.
 *
 * The Rust `Thread` struct is wire-compatible with `src/state/threads.ts`'s
 * `Thread` interface — `messages` is passed through as a raw JSON `Value`
 * on the Rust side so the frontend owns the message schema.
 *
 * If we're not running inside Tauri (e.g. a Vite browser-only dev server),
 * `invoke` throws — every helper here swallows that into an empty-ish result
 * so the UI degrades gracefully rather than crashing the whole panel.
 */

import { invoke } from "@tauri-apps/api/core";
import type { Thread } from "@/state/threads";

/**
 * Used when no project is active — keeps a single global thread bucket. The
 * Rust side maps this sentinel onto the user's home directory, so global
 * threads persist under `~/.cortex/threads/` and survive restarts exactly
 * like project-scoped ones.
 */
export const GLOBAL_PROJECT_ROOT_KEY = "__cortex_global__";

/**
 * Resolve a usable `project_root` argument. With an active project this is
 * its real root directory; without one we fall back to the global sentinel
 * so persistence still works (no more threads evaporating on restart).
 */
export function resolveProjectRoot(activeRoot: string | null | undefined): string {
  if (!activeRoot || typeof activeRoot !== "string") return GLOBAL_PROJECT_ROOT_KEY;
  return activeRoot;
}

export async function listThreads(projectRoot: string | null): Promise<Thread[]> {
  if (!projectRoot) return [];
  try {
    return await invoke<Thread[]>("list_threads", { projectRoot });
  } catch (err) {
    console.warn("listThreads failed", err);
    return [];
  }
}

export async function saveThread(projectRoot: string | null, thread: Thread): Promise<void> {
  if (!projectRoot) return;
  try {
    await invoke("save_thread", { projectRoot, thread });
  } catch (err) {
    console.warn("saveThread failed", err);
  }
}

export async function deleteThread(projectRoot: string | null, id: string): Promise<void> {
  if (!projectRoot) return;
  try {
    await invoke("delete_thread", { projectRoot, id });
  } catch (err) {
    console.warn("deleteThread failed", err);
  }
}

/**
 * Convenience: the Rust side has no dedicated `load_thread`; `list_threads`
 * returns all threads sorted most-recent first, so a single load is just a
 * `.find()` after a list. Exposed as its own function so the call sites read
 * intentionally and we can specialise the backend later without churn.
 */
export async function loadThread(projectRoot: string | null, id: string): Promise<Thread | null> {
  const all = await listThreads(projectRoot);
  return all.find((t) => t.id === id) ?? null;
}

/**
 * Derive a human-readable title: a user-set custom title (inline rename)
 * always wins; otherwise the first user message (max 60 chars), then the
 * auto label.
 */
export function deriveThreadTitle(thread: Thread): string {
  const custom = thread.customTitle?.trim();
  if (custom) return custom;
  const firstUser = thread.messages.find((m) => m.role === "user");
  const raw = firstUser?.content?.trim();
  if (raw && raw.length > 0) {
    return raw.length > 60 ? raw.slice(0, 60) + "…" : raw;
  }
  return thread.label || `thread ${thread.id.slice(-6)}`;
}
