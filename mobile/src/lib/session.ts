// Which Cortex we talk to and how we prove who we are. Persisted across
// launches: the server URL (native shell only — the PWA is same-origin), the
// bearer token (secure storage via the native bridge when available) and the
// pairing metadata shown on the More screen.

import {
  isNativeShell,
  prefGet,
  prefSet,
  secretGet,
  secretSet,
} from "./native";

const KEY_SERVER = "cortex.server"; // { url, device_id, server_name, server_version }
const KEY_TOKEN = "cortex.token";

export interface ServerInfo {
  /** Base URL, e.g. "https://desktop.tail1234.ts.net". Empty = same origin. */
  url: string;
  device_id?: string;
  server_name?: string;
  server_version?: string;
}

type Listener = () => void;

class Session {
  private info: ServerInfo = prefGet<ServerInfo>(KEY_SERVER, { url: "" });
  private tok: string | null = null;
  private loaded = false;
  private unauthorizedListeners = new Set<Listener>();

  /** Load the token from secure storage. Call once at boot. */
  async load() {
    if (this.loaded) return;
    this.tok = await secretGet(KEY_TOKEN);
    this.loaded = true;
  }

  baseUrl(): string {
    return this.info.url || "";
  }

  server(): ServerInfo {
    return this.info;
  }

  token(): string | null {
    return this.tok;
  }

  /** Same-origin PWA (served by Cortex) vs native shell needing a URL. */
  needsServerUrl(): boolean {
    return isNativeShell() && !this.info.url;
  }

  async setServer(info: ServerInfo, token: string | null) {
    this.info = info;
    prefSet(KEY_SERVER, info);
    this.tok = token;
    await secretSet(KEY_TOKEN, token);
  }

  async clear() {
    // Keep the URL in the native shell so re-pairing prefills it.
    this.info = { url: isNativeShell() ? this.info.url : "" };
    prefSet(KEY_SERVER, this.info);
    this.tok = null;
    await secretSet(KEY_TOKEN, null);
  }

  onUnauthorized(fn: Listener): () => void {
    this.unauthorizedListeners.add(fn);
    return () => this.unauthorizedListeners.delete(fn);
  }

  notifyUnauthorized() {
    for (const l of this.unauthorizedListeners) l();
  }
}

export const session = new Session();

/** Parse a pairing payload: `cortex://pair?url=<base>&code=<6 digits>`. Also
 *  accepts a bare 6-digit code (URL must then be supplied by the user). */
export function parsePairLink(
  raw: string,
): { url?: string; code?: string } | null {
  const s = raw.trim();
  if (!s) return null;
  if (/^\d{6}$/.test(s)) return { code: s };
  const m = /^cortex:\/\/pair\??(.*)$/i.exec(s) || /^#?pair\?(.*)$/i.exec(s);
  if (!m) return null;
  const params = new URLSearchParams(m[1]);
  const url = params.get("url") ?? undefined;
  const code = params.get("code") ?? undefined;
  return { url: url ? normaliseUrl(url) : undefined, code };
}

/** "desktop.ts.net:8788" → "http://desktop.ts.net:8788"; strips trailing "/". */
export function normaliseUrl(u: string): string {
  let s = u.trim();
  if (!s) return "";
  if (!/^https?:\/\//i.test(s)) s = `http://${s}`;
  return s.replace(/\/+$/, "");
}
