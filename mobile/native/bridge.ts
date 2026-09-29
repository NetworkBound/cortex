// Native bridge for the Cortex mobile app.
//
// The web app (mobile/src) never imports Capacitor. Instead this file is
// bundled by scripts/mobile/build-bridge.mjs into mobile/dist/native-bridge.js
// and injected as a classic <script> ahead of the SPA's module script, so by
// the time React boots `window.CortexNative` exists on a phone and is
// `undefined` in a plain browser / PWA. The client feature-detects it.
//
// Contract (superset of what mobile/src/lib/native.ts consumes — keep in sync):
//   platform, haptic(kind), scanQr(), getSecret/setSecret(key, value|null),
//   secureGet/secureSet/secureDelete(key) (aliases), deviceName(),
//   openExternal(url), share(text), setStatusBar(style)
// plus additive extras (onDeepLink, getLaunchUrl, hideSplash, hideKeyboard).
// DOM events on window: "cortex:deeplink" (CustomEvent<string> — the URL,
// also fired for the cold-start URL after load), "cortex:resume",
// "cortex:pause".
//
// `secure*` is backed by @capacitor/preferences (UserDefaults / SharedPrefs):
// it is per-app sandboxed but NOT encrypted at rest. The upgrade path is a
// keychain/keystore plugin (e.g. capacitor-secure-storage-plugin) behind the
// same three functions — nothing in the client has to change.

import { Capacitor } from "@capacitor/core";
import { App } from "@capacitor/app";
import { Browser } from "@capacitor/browser";
import { Device } from "@capacitor/device";
import { Haptics, ImpactStyle, NotificationType } from "@capacitor/haptics";
import { Keyboard } from "@capacitor/keyboard";
import { Preferences } from "@capacitor/preferences";
import { Share } from "@capacitor/share";
import { SplashScreen } from "@capacitor/splash-screen";
import { StatusBar, Style } from "@capacitor/status-bar";
import {
  BarcodeFormat,
  BarcodeScanner,
} from "@capacitor-mlkit/barcode-scanning";

export type HapticKind =
  | "light"
  | "medium"
  | "heavy"
  | "success"
  | "warning"
  | "error"
  | "selection";

export type StatusBarStyle = "light" | "dark";

export interface CortexNativeBridge {
  /** "ios" | "android" (never "web": the bridge is only installed natively). */
  platform: "ios" | "android";
  /** Bridge/app version (mirrors the root package.json version at build time). */
  version: string;
  haptic(kind: HapticKind): Promise<void>;
  /** Open the camera QR scanner; resolves with the raw QR text or null if cancelled/unavailable. */
  scanQr(): Promise<string | null>;
  getSecret(key: string): Promise<string | null>;
  /** `null` deletes the key. */
  setSecret(key: string, value: string | null): Promise<void>;
  /** Aliases of getSecret/setSecret/setSecret(key, null). */
  secureGet(key: string): Promise<string | null>;
  secureSet(key: string, value: string): Promise<void>;
  secureDelete(key: string): Promise<void>;
  /** Human device name for pairing, e.g. "Connor's iPhone" / "Pixel 8". */
  deviceName(): Promise<string>;
  /** Open a URL in the system browser (SFSafariViewController / Custom Tab). */
  openExternal(url: string): Promise<void>;
  share(text: string, title?: string): Promise<void>;
  setStatusBar(style: StatusBarStyle): Promise<void>;
  // --- extras -------------------------------------------------------------
  /** Subscribe to `cortex://...` deep links opened while the app is running. Returns an unsubscribe fn. */
  onDeepLink(cb: (url: string) => void): () => void;
  /** The `cortex://...` URL the app was cold-started with, if any. */
  getLaunchUrl(): Promise<string | null>;
  /** Hide the splash screen early (it auto-hides after ~600 ms anyway). */
  hideSplash(): Promise<void>;
  /** Dismiss the soft keyboard. */
  hideKeyboard(): Promise<void>;
}

declare global {
  interface Window {
    CortexNative?: CortexNativeBridge;
  }
}

declare const __CORTEX_VERSION__: string;

const PREF_PREFIX = "cortex.secure.";

function swallow(p: Promise<unknown>): Promise<void> {
  return p.then(
    () => undefined,
    (e) => {
      console.warn("[CortexNative]", e);
    },
  );
}

async function haptic(kind: HapticKind): Promise<void> {
  switch (kind) {
    case "light":
      return swallow(Haptics.impact({ style: ImpactStyle.Light }));
    case "medium":
      return swallow(Haptics.impact({ style: ImpactStyle.Medium }));
    case "heavy":
      return swallow(Haptics.impact({ style: ImpactStyle.Heavy }));
    case "success":
      return swallow(Haptics.notification({ type: NotificationType.Success }));
    case "warning":
      return swallow(Haptics.notification({ type: NotificationType.Warning }));
    case "error":
      return swallow(Haptics.notification({ type: NotificationType.Error }));
    case "selection":
      return swallow(Haptics.selectionChanged());
    default:
      return;
  }
}

/**
 * Last-resort QR path when the ML Kit plugin is missing or fails: take a photo
 * with the system camera (`<input capture>`) and decode it with the browser's
 * BarcodeDetector (available in the Android System WebView; absent on iOS,
 * where the plugin is the only route). Resolves null when nothing decodes.
 */
function scanQrFallback(): Promise<string | null> {
  type Detector = {
    detect(src: ImageBitmapSource): Promise<Array<{ rawValue: string }>>;
  };
  type DetectorCtor = new (opts: { formats: string[] }) => Detector;
  const Ctor = (window as unknown as { BarcodeDetector?: DetectorCtor })
    .BarcodeDetector;
  if (!Ctor) return Promise.resolve(null);

  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/*";
    input.setAttribute("capture", "environment");
    input.style.display = "none";
    let settled = false;
    const finish = (v: string | null) => {
      if (settled) return;
      settled = true;
      input.remove();
      resolve(v);
    };
    input.addEventListener("change", async () => {
      const file = input.files?.[0];
      if (!file) return finish(null);
      try {
        const bitmap = await createImageBitmap(file);
        const found = await new Ctor({ formats: ["qr_code"] }).detect(bitmap);
        bitmap.close();
        finish(found[0]?.rawValue ?? null);
      } catch (e) {
        console.warn("[CortexNative] BarcodeDetector failed", e);
        finish(null);
      }
    });
    // The picker was dismissed without a file: `change` never fires, but
    // focus returns to the page. Give iOS/Android a beat to deliver `change`.
    window.addEventListener(
      "focus",
      () => {
        setTimeout(() => finish(null), 1500);
      },
      { once: true },
    );
    document.body.appendChild(input);
    input.click();
  });
}

async function scanQr(): Promise<string | null> {
  if (!Capacitor.isPluginAvailable("BarcodeScanner")) {
    return scanQrFallback();
  }
  try {
    const { supported } = await BarcodeScanner.isSupported();
    if (!supported) return scanQrFallback();

    if (Capacitor.getPlatform() === "android") {
      // `scan()` on Android uses the Google code-scanner module (no camera
      // permission needed); it may have to be downloaded once.
      const { available } =
        await BarcodeScanner.isGoogleBarcodeScannerModuleAvailable();
      if (!available) {
        await BarcodeScanner.installGoogleBarcodeScannerModule();
        // Installation is async; the plugin fires a progress event. Fall back
        // for this attempt so the user isn't left waiting.
        return scanQrFallback();
      }
    } else {
      const perm = await BarcodeScanner.requestPermissions();
      if (perm.camera !== "granted" && perm.camera !== "limited") return null;
    }

    const { barcodes } = await BarcodeScanner.scan({
      formats: [BarcodeFormat.QrCode],
    });
    const first = barcodes[0];
    return first ? first.rawValue || first.displayValue || null : null;
  } catch (e) {
    // User cancelled → the plugin rejects; treat as "no result".
    const msg = String((e as { message?: string })?.message ?? e).toLowerCase();
    if (msg.includes("cancel")) return null;
    console.warn("[CortexNative] scan failed, trying fallback", e);
    return scanQrFallback();
  }
}

async function secureGet(key: string): Promise<string | null> {
  const { value } = await Preferences.get({ key: PREF_PREFIX + key });
  return value ?? null;
}

function secureSet(key: string, value: string): Promise<void> {
  return Preferences.set({ key: PREF_PREFIX + key, value });
}

function secureDelete(key: string): Promise<void> {
  return Preferences.remove({ key: PREF_PREFIX + key });
}

function setSecret(key: string, value: string | null): Promise<void> {
  return value === null ? secureDelete(key) : secureSet(key, value);
}

async function deviceName(): Promise<string> {
  try {
    const info = await Device.getInfo();
    // `name` is the user-set name on iOS (and Android 7.1+ where exposed);
    // otherwise fall back to manufacturer + model.
    if (info.name) return info.name;
    const parts = [info.manufacturer, info.model].filter(Boolean);
    if (parts.length) return parts.join(" ");
    return info.platform === "ios" ? "iPhone" : "Android phone";
  } catch {
    return Capacitor.getPlatform() === "ios" ? "iPhone" : "Android phone";
  }
}

async function openExternal(url: string): Promise<void> {
  if (!/^https?:\/\//i.test(url)) {
    // mailto:, tel:, other apps' schemes → let the OS route it.
    window.open(url, "_system");
    return;
  }
  await Browser.open({ url, presentationStyle: "popover" });
}

async function share(text: string, title?: string): Promise<void> {
  const { value: canShare } = await Share.canShare();
  if (!canShare) {
    await navigator.clipboard?.writeText(text);
    return;
  }
  try {
    await Share.share({ title, text, dialogTitle: title });
  } catch (e) {
    // Dismissing the share sheet rejects on some OS versions; not an error.
    console.debug("[CortexNative] share dismissed", e);
  }
}

async function setStatusBar(style: StatusBarStyle): Promise<void> {
  // "dark" = dark background, light icons (Style.Dark); "light" = the reverse.
  await swallow(
    StatusBar.setStyle({ style: style === "dark" ? Style.Dark : Style.Light }),
  );
  if (Capacitor.getPlatform() === "android") {
    await swallow(
      StatusBar.setBackgroundColor({
        color: style === "dark" ? "#0a0a0b" : "#ffffff",
      }),
    );
  }
}

const deepLinkListeners = new Set<(url: string) => void>();

function onDeepLink(cb: (url: string) => void): () => void {
  deepLinkListeners.add(cb);
  return () => {
    deepLinkListeners.delete(cb);
  };
}

async function getLaunchUrl(): Promise<string | null> {
  try {
    const res = await App.getLaunchUrl();
    return res?.url ?? null;
  } catch {
    return null;
  }
}

function install(): void {
  const platform = Capacitor.getPlatform();
  if (platform !== "ios" && platform !== "android") {
    // Running in a normal browser (e.g. `vite preview` with the bridge
    // injected): leave `window.CortexNative` undefined so the client takes
    // its web paths.
    return;
  }

  const bridge: CortexNativeBridge = {
    platform,
    version:
      typeof __CORTEX_VERSION__ === "string" ? __CORTEX_VERSION__ : "0.0.0",
    haptic,
    scanQr,
    getSecret: secureGet,
    setSecret,
    secureGet,
    secureSet,
    secureDelete,
    deviceName,
    openExternal,
    share,
    setStatusBar,
    onDeepLink,
    getLaunchUrl,
    hideSplash: () => swallow(SplashScreen.hide()),
    hideKeyboard: () => swallow(Keyboard.hide()),
  };
  window.CortexNative = bridge;

  // Deep links (`cortex://pair?...`, `cortex://inbox`) arriving while running.
  // Also re-broadcast as a DOM event so code that doesn't hold the bridge can
  // listen with `window.addEventListener("cortex:deeplink", ...)`.
  void App.addListener("appUrlOpen", ({ url }: { url: string }) => {
    for (const cb of deepLinkListeners) {
      try {
        cb(url);
      } catch (e) {
        console.warn("[CortexNative] deep link handler threw", e);
      }
    }
    window.dispatchEvent(
      new CustomEvent("cortex:deeplink", { detail: { url } }),
    );
  });

  // Android hardware back: walk the SPA's history, exit only from the root.
  if (platform === "android") {
    void App.addListener(
      "backButton",
      ({ canGoBack }: { canGoBack: boolean }) => {
        if (canGoBack) {
          window.history.back();
        } else {
          void App.exitApp();
        }
      },
    );
  }

  void setStatusBar("dark");
}

install();
