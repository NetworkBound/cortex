import { invoke } from "@tauri-apps/api/core";

/**
 * Lightweight update-check result returned by the Rust `check_updates`
 * command. We do NOT download or apply updates here — this is only used
 * to show a small "↑ update" pill in the status bar and a note in
 * Settings → Updates.
 */
export interface UpdateInfo {
  current: string;
  latest: string;
  available: boolean;
  notes: string | null;
  url: string | null;
}

/**
 * Update-manifest URL. There is NO baked-in default: shipped builds must not
 * carry deployment-specific (LAN/VPN) addresses. The URL is configured per
 * machine in localStorage under `cortex.updateUrl`; when unset, the update
 * check is simply "not configured" and performs no network I/O.
 *
 * When set, it is fetched over HTTPS: the manifest is the source of truth for
 * whether an update is available (and the download URL/notes shown to the
 * user), so it must not be tamperable by a network attacker on a cleartext
 * channel.
 */
/**
 * Update source: a releases API URL (Gitea or GitHub, newest-first) or a
 * `{version,…}` manifest — the Rust `check_updates` understands both. There is
 * no baked-in default (a URL would embed one deployment's infrastructure into
 * every build); set it per-machine via localStorage `cortex.updateUrl`. When
 * unset, the update check is simply "not configured" and performs no I/O.
 */
export function configuredManifestUrl(): string | null {
  try {
    const stored = localStorage.getItem("cortex.updateUrl");
    if (stored && stored.trim().length > 0) return stored.trim();
  } catch {
    /* private mode — treated as unconfigured */
  }
  return null;
}

/** A host on this machine or a private LAN (RFC1918 / link-local). */
function isPrivateOrLoopback(hostname: string): boolean {
  if (hostname === "localhost" || hostname === "127.0.0.1" || hostname === "::1") return true;
  const m = hostname.match(/^(\d+)\.(\d+)\.\d+\.\d+$/);
  if (!m) return false;
  const a = Number(m[1]);
  const b = Number(m[2]);
  return (
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 169 && b === 254)
  );
}

/**
 * Guard the update fetch transport. `https:` is always fine. Cleartext `http:`
 * is allowed ONLY for loopback or a private-LAN host (the user's own network,
 * e.g. a self-hosted Gitea with no TLS) — never for a public-internet host,
 * where a MITM could forge update info.
 */
function assertSecureManifestUrl(manifestUrl: string): void {
  let parsed: URL;
  try {
    parsed = new URL(manifestUrl);
  } catch {
    throw new Error(`Invalid update manifest URL: ${manifestUrl}`);
  }
  if (parsed.protocol === "https:") return;
  if (parsed.protocol === "http:" && isPrivateOrLoopback(parsed.hostname)) return;
  throw new Error(
    `Refusing to fetch update info over insecure transport (${parsed.protocol}//). ` +
      `Use https:// or a private-LAN/loopback host.`,
  );
}

export async function checkUpdates(manifestUrl: string): Promise<UpdateInfo> {
  assertSecureManifestUrl(manifestUrl);
  return invoke<UpdateInfo>("check_updates", { manifestUrl });
}
