// Bridge to the native Capacitor shell. Everything here is optional: the PWA
// runs without it (served straight from the Cortex server), and the shell
// installs `window.CortexNative` before the bundle loads to add haptics, QR
// scanning, secure token storage and deep links.
//
// Contract (implemented by mobile/native, consumed here — keep in sync with
// reports4/mobile-client.md):
//
//   interface CortexNative {
//     platform: "ios" | "android";
//     haptic(kind?: HapticKind): void;
//     scanQr(): Promise<string | null>;        // decoded text, null if cancelled
//     getSecret(key: string): Promise<string | null>;
//     setSecret(key: string, value: string | null): Promise<void>;
//     deviceName(): Promise<string>;           // e.g. "Connor's iPhone"
//     openExternal(url: string): void;
//   }
//
// The shell dispatches these DOM events on `window`:
//   "cortex:deeplink"  CustomEvent<string>  — a cortex:// URL to handle
//   "cortex:resume" / "cortex:pause"        — app foreground / background

export type HapticKind =
  | "light"
  | "medium"
  | "heavy"
  | "selection"
  | "success"
  | "warning"
  | "error";

export interface CortexNative {
  platform: "ios" | "android";
  haptic?: (kind?: HapticKind) => void;
  scanQr?: () => Promise<string | null>;
  getSecret?: (key: string) => Promise<string | null>;
  setSecret?: (key: string, value: string | null) => Promise<void>;
  deviceName?: () => Promise<string>;
  openExternal?: (url: string) => void;
}

declare global {
  interface Window {
    CortexNative?: CortexNative;
  }
}

export function native(): CortexNative | undefined {
  return typeof window !== "undefined" ? window.CortexNative : undefined;
}

/** True when running inside the native shell (or any non-http origin such
 *  as `capacitor://localhost`), i.e. the API is NOT same-origin. */
export function isNativeShell(): boolean {
  if (native()) return true;
  const p = location.protocol;
  return p !== "http:" && p !== "https:";
}

const VIBE: Record<HapticKind, number | number[]> = {
  light: 8,
  medium: 15,
  heavy: 25,
  selection: 5,
  success: [10, 40, 10],
  warning: [20, 40, 20],
  error: [30, 50, 30, 50, 30],
};

/** Fire a haptic: native bridge when present, else the Vibration API where
 *  the browser exposes it (Android Chrome). Never throws. */
export function haptic(kind: HapticKind = "light") {
  try {
    const n = native();
    if (n?.haptic) {
      n.haptic(kind);
      return;
    }
    if (typeof navigator !== "undefined" && "vibrate" in navigator) {
      navigator.vibrate(VIBE[kind]);
    }
  } catch {
    /* haptics are best-effort */
  }
}

/** Secure-ish key/value: the native shell backs it with Keychain/Keystore;
 *  the PWA falls back to localStorage. All calls are best-effort. */
export async function secretGet(key: string): Promise<string | null> {
  const n = native();
  if (n?.getSecret) {
    try {
      return await n.getSecret(key);
    } catch {
      /* fall through */
    }
  }
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export async function secretSet(key: string, value: string | null) {
  const n = native();
  if (n?.setSecret) {
    try {
      await n.setSecret(key, value);
      return;
    } catch {
      /* fall through */
    }
  }
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* quota / private mode */
  }
}

/** Plain (non-secret) persisted prefs. */
export function prefGet<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

export function prefSet(key: string, value: unknown) {
  try {
    if (value === undefined || value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* ignore */
  }
}

/** Best-effort human device name for pairing. */
export async function deviceName(): Promise<string> {
  const n = native();
  if (n?.deviceName) {
    try {
      const s = await n.deviceName();
      if (s) return s;
    } catch {
      /* fall through */
    }
  }
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua)) return "iPad";
  if (/Android/.test(ua)) return "Android phone";
  return "Browser";
}

export function openExternal(url: string) {
  const n = native();
  if (n?.openExternal) {
    n.openExternal(url);
    return;
  }
  window.open(url, "_blank", "noopener");
}
