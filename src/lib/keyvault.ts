import { invoke } from "@tauri-apps/api/core";
import { timeAgo } from "@/lib/time";

/**
 * Bridge for the AES-GCM-encrypted provider key vault. Mirrors the Rust API:
 *  - `vaultList`   — metadata only, never returns the key bytes
 *  - `vaultGet`    — pulls one key by (provider, label); call sparingly
 *  - `vaultSet`    — upsert
 *  - `vaultRemove` — hard delete
 */

export interface KeyMetadata {
  provider: string;
  label: string;
  added_unix_ms: number;
}

/** Curated provider list for the dropdown; users can still type anything. */
export const KNOWN_PROVIDERS: string[] = [
  "anthropic",
  "openai",
  "google",
  "groq",
  "deepseek",
  "openrouter",
  "mistral",
  "fireworks",
  "xai",
];

export async function vaultList(): Promise<KeyMetadata[]> {
  return invoke<KeyMetadata[]>("vault_list");
}

export async function vaultGet(provider: string, label: string): Promise<string> {
  return invoke<string>("vault_get", { provider, label });
}

export async function vaultSet(
  provider: string,
  label: string,
  key: string,
): Promise<void> {
  return invoke("vault_set", { provider, label, key });
}

export async function vaultRemove(provider: string, label: string): Promise<void> {
  return invoke("vault_remove", { provider, label });
}

/** Relative, but keys added >30d ago read as an absolute date. */
export function formatAddedAt(unixMs: number): string {
  return timeAgo(unixMs, { empty: "unknown", absoluteAfterDays: 30 });
}
