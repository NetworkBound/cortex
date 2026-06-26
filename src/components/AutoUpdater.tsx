import { useEffect, useRef } from "react";
import {
  applyReleaseUpdate,
  checkReleaseUpdate,
} from "@/lib/self-update";
import { pushToast } from "@/lib/toast";

/**
 * Background self-updater (Linux AppImage only). Shortly after boot, then every
 * 30 minutes, it asks the backend whether the Gitea release has a newer
 * AppImage and — if so — downloads and swaps it in place. The new version
 * applies on the next launch, so we surface a one-time toast nudging a restart.
 *
 * Renders nothing. Fully best-effort: on any non-AppImage build the backend
 * reports `supported: false` and this no-ops, and every error is swallowed so
 * the updater can never disrupt the running app.
 */

const FIRST_CHECK_MS = 8_000; // let startup settle first
const INTERVAL_MS = 30 * 60 * 1_000;

export function AutoUpdater() {
  const busy = useRef(false);

  useEffect(() => {
    let cancelled = false;

    async function tick() {
      if (busy.current) return;
      busy.current = true;
      try {
        const info = await checkReleaseUpdate();
        if (
          cancelled ||
          !info.supported ||
          !info.available ||
          !info.download_url ||
          !info.latest_key
        ) {
          return;
        }
        const res = await applyReleaseUpdate(info.download_url, info.latest_key);
        if (cancelled || res !== "applied") return;
        pushToast({
          title: `Cortex updated${info.tag ? ` to ${info.tag}` : ""}`,
          body: "Restart Cortex to apply the new version.",
          kind: "success",
          ttlMs: 15_000,
        });
      } catch {
        /* best-effort — never disrupt the app */
      } finally {
        busy.current = false;
      }
    }

    const first = setTimeout(tick, FIRST_CHECK_MS);
    const iv = setInterval(tick, INTERVAL_MS);
    return () => {
      cancelled = true;
      clearTimeout(first);
      clearInterval(iv);
    };
  }, []);

  return null;
}
