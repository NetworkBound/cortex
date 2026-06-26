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
export function configuredManifestUrl(): string | null {
  try {
    const stored = localStorage.getItem("cortex.updateUrl");
    if (stored && stored.trim().length > 0) return stored.trim();
  } catch {
    /* private mode — treat as unconfigured */
  }
  return null;
}

/**
 * Guard against fetching the update manifest over an insecure transport.
 * Because the URL is user-repointable (localStorage `cortex.updateUrl`), a
 * cleartext `http://` manifest would let a man-in-the-middle forge update
 * info. We require `https:` for any non-loopback host.
 */
function assertSecureManifestUrl(manifestUrl: string): void {
  let parsed: URL;
  try {
    parsed = new URL(manifestUrl);
  } catch {
    throw new Error(`Invalid update manifest URL: ${manifestUrl}`);
  }

  const isLoopback =
    parsed.hostname === "localhost" ||
    parsed.hostname === "127.0.0.1" ||
    parsed.hostname === "::1";

  if (parsed.protocol !== "https:" && !isLoopback) {
    throw new Error(
      `Refusing to fetch update manifest over insecure transport (${parsed.protocol}//). ` +
        `Use https:// or a loopback host.`,
    );
  }
}

export async function checkUpdates(manifestUrl: string): Promise<UpdateInfo> {
  assertSecureManifestUrl(manifestUrl);
  return invoke<UpdateInfo>("check_updates", { manifestUrl });
}
