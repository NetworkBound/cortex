import { invoke } from "@tauri-apps/api/core";

/**
 * Mirror of `commands::channels::*` — multi-agent persistent rooms backed by
 * `~/.cortex/channels/<id>.json`. A channel is a Slack-style room with N
 * member specs (user + one-or-more `agent_role` personas drawn from
 * `~/.cortex/roles/*.yaml`); `@role-name` in a posted message summons that
 * role via the gateway and appends its reply.
 */

export interface MemberSpec {
  /** `"user"` or `"agent_role"`. */
  kind: "user" | "agent_role";
  id: string;
  name: string;
}

export interface ChannelMessage {
  id: string;
  author_kind: string;
  author_id: string;
  content: string;
  ts: number;
  mentions: string[];
}

export interface Channel {
  id: string;
  name: string;
  description: string;
  members: MemberSpec[];
  messages: ChannelMessage[];
  created_unix_ms: number;
}

export async function listChannels(): Promise<Channel[]> {
  return invoke<Channel[]>("list_channels");
}

export async function getChannel(id: string): Promise<Channel> {
  return invoke<Channel>("get_channel", { id });
}

export async function createChannel(
  name: string,
  description: string,
  members: MemberSpec[],
): Promise<Channel> {
  return invoke<Channel>("create_channel", { name, description, members });
}

export async function deleteChannel(id: string): Promise<void> {
  return invoke("delete_channel", { id });
}

/**
 * Returns the newly-appended messages (user post + one reply per mention).
 * While in flight the backend streams per-role progress over the
 * `channels:progress:<channelId>` event: `start` → `delta`* (token chunks) →
 * `done`/`error` (final reply text) — see `ChannelProgress` in ChannelsPanel.
 */
export async function postMessage(
  channelId: string,
  content: string,
): Promise<ChannelMessage[]> {
  return invoke<ChannelMessage[]>("post_message", { channelId, content });
}

/** Stable per-role colour so transcript bubbles stay visually pinned to an
 *  author across renders. Pure hash → HSL, no external palette state. */
export function colorForAuthor(id: string): string {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  return `hsl(${hue} 55% 45%)`;
}

/** Pluck `@role-name` tokens out of a draft so the composer can highlight
 *  unknown mentions before the user hits send. Mirrors the backend regex. */
export function extractMentions(text: string): string[] {
  const out: string[] = [];
  const re = /(^|[^A-Za-z0-9_])@([A-Za-z0-9_.-]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (!out.includes(m[2])) out.push(m[2]);
  }
  return out;
}

/** Open a channel by name, creating it (user-only) if missing. Returns the
 *  resolved channel id so callers can pre-select it in the panel. */
export async function openOrCreateChannelByName(name: string): Promise<Channel> {
  const all = await listChannels();
  const existing = all.find(
    (c) => c.name.toLowerCase() === name.trim().toLowerCase(),
  );
  if (existing) return existing;
  return createChannel(name, "", [{ kind: "user", id: "user", name: "You" }]);
}
