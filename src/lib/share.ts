import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::share::ShareMessage`. We map the in-memory
 * chat `Message` shape down to just the fields that render meaningfully in
 * markdown so the backend stays decoupled from store-internal additions.
 */
export interface ShareMessage {
  role: string;
  agent?: string | null;
  content: string;
  /** Unix epoch milliseconds. Optional — falsy values are treated as "unknown". */
  ts_unix_ms?: number | null;
}

/**
 * Render the conversation as Markdown. When `target` is supplied the backend
 * writes the file (must live under `~/Documents/Cortex Brain` or the active
 * project root) and *also* returns the markdown so the caller can copy it.
 *
 * @param messages    Messages, in chronological order.
 * @param target      Absolute path to write to (optional).
 * @param projectRoot Active project root, used by the backend to allow-list
 *                    paths that live inside the current project.
 */
export async function shareChatAsMarkdown(
  messages: ShareMessage[],
  target?: string | null,
  projectRoot?: string | null,
): Promise<string> {
  return invoke<string>("share_chat_as_markdown", {
    messages,
    target: target ?? null,
    activeProjectRoot: projectRoot ?? null,
  });
}
