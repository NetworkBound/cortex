import { invoke } from "@tauri-apps/api/core";

/**
 * Per-chat metadata (custom title, favorite flag, tags) keyed by transcript
 * `file_path`. Backed by `~/.cortex/chat-meta.json` via the Tauri commands
 * defined in `src-tauri/src/commands/chat_meta.rs`. The sidebar fetches the
 * full map once on mount via {@link listChatMeta} and mutates per-row via
 * {@link setChatMeta}.
 */
export interface ChatMeta {
  custom_title?: string | null;
  is_favorite: boolean;
  tags: string[];
}

export const EMPTY_CHAT_META: ChatMeta = {
  custom_title: null,
  is_favorite: false,
  tags: [],
};

export async function getChatMeta(filePath: string): Promise<ChatMeta | null> {
  const m = await invoke<ChatMeta | null>("get_chat_meta", { filePath });
  return m ?? null;
}

export async function setChatMeta(
  filePath: string,
  meta: ChatMeta,
): Promise<void> {
  await invoke<void>("set_chat_meta", { filePath, meta });
}

export async function listChatMeta(): Promise<Record<string, ChatMeta>> {
  return invoke<Record<string, ChatMeta>>("list_chat_meta");
}

/**
 * Parse a comma-separated string into a trimmed, deduped, capped tag list
 * suitable for {@link setChatMeta}. Mirrors the sanitizer in `chat_meta.rs`
 * so the UI doesn't surprise the user with backend rejections.
 */
export function parseTagsInput(raw: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const part of raw.split(",")) {
    const t = part.trim().slice(0, 64);
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push(t);
    if (out.length >= 16) break;
  }
  return out;
}
