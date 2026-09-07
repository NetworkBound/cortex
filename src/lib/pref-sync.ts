/**
 * Disk-backed mirror of the `cortex.*` localStorage prefs.
 *
 * The backend wipes localStorage on every app-version change (post-update
 * cache-bust via `clear_all_browsing_data`), which would otherwise reset the
 * onboarding flag, sidebar/nav widths, prompt history, theme, etc. on each
 * update. This module mirrors those prefs to `~/.cortex/ui-prefs.json` and
 * restores any that are missing at boot — so prefs survive updates.
 *
 * Generic by design: it persists the entire `cortex.*` key namespace, so new
 * pref keys are covered automatically with no whitelist to maintain.
 */
import { invoke } from "@tauri-apps/api/core";

const PREFIX = "cortex.";

/**
 * Guard against a tampered `ui-prefs.json` injecting arbitrary entries into
 * localStorage. A legitimate pref key is `cortex.` followed by a bounded run of
 * word/dot/dash chars; values and overall entry count are also capped so a
 * malicious file can't flood storage. These bounds are intentionally generous
 * so real prefs (history blobs, layout state) keep working.
 */
const MAX_PREF_ENTRIES = 256;
const MAX_VALUE_BYTES = 1_000_000;
const KEY_RE = /^cortex\.[\w.-]{1,128}$/;

function isValidPrefKey(k: string): boolean {
  return KEY_RE.test(k);
}

/** Snapshot every `cortex.*` localStorage entry into a plain object. */
function snapshot(): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k && k.startsWith(PREFIX)) {
      const v = localStorage.getItem(k);
      if (v !== null) out[k] = v;
    }
  }
  return out;
}

/**
 * Restore persisted prefs that are missing from localStorage (e.g. after a
 * post-update wipe). Never overwrites a value already present this session, so
 * it's a no-op on a normal launch. Call BEFORE React mounts so components read
 * the restored values.
 */
export async function restorePrefsAtBoot(): Promise<void> {
  try {
    const raw = await invoke<string>("read_ui_prefs");
    const disk = JSON.parse(raw) as Record<string, unknown>;
    let restored = 0;
    for (const [k, v] of Object.entries(disk)) {
      if (restored >= MAX_PREF_ENTRIES) break;
      if (
        isValidPrefKey(k) &&
        typeof v === "string" &&
        v.length <= MAX_VALUE_BYTES &&
        localStorage.getItem(k) === null
      ) {
        localStorage.setItem(k, v);
        restored++;
      }
    }
  } catch {
    /* best-effort — prefs are a nice-to-have, never block boot */
  }
}

async function mirror(): Promise<void> {
  try {
    await invoke("write_ui_prefs", { json: JSON.stringify(snapshot()) });
  } catch {
    /* ignore */
  }
}

/**
 * Keep the disk mirror current so the latest prefs are captured before the next
 * launch (which may wipe localStorage). Mirrors immediately, on a 10s timer, on
 * tab-hide, and on unload. Returns a cleanup fn.
 */
export function attachPrefMirror(): () => void {
  void mirror();
  const id = setInterval(() => void mirror(), 10_000);
  const onVisibility = () => {
    if (document.visibilityState === "hidden") void mirror();
  };
  const onUnload = () => void mirror();
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("beforeunload", onUnload);
  return () => {
    clearInterval(id);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("beforeunload", onUnload);
  };
}
