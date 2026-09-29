// Web push (iPhone PWA + Android). The service worker (public/sw.js) shows
// notifications; this module handles permission, VAPID subscription and
// registering the subscription with the server. Every step is feature
// detected; nothing here throws on browsers without push.

import { get, post } from "./http";
import { session } from "./session";
import { deviceName } from "./native";

export type PushState =
  | "unsupported"
  | "default"
  | "granted"
  | "denied"
  | "subscribed";

export function swSupported(): boolean {
  return (
    typeof navigator !== "undefined" &&
    "serviceWorker" in navigator &&
    (location.protocol === "https:" || location.hostname === "localhost")
  );
}

export function pushSupported(): boolean {
  return (
    swSupported() &&
    typeof window !== "undefined" &&
    "PushManager" in window &&
    "Notification" in window
  );
}

/** Register the hand-written service worker (production only — the dev
 *  server has no sw.js and would cache stale modules). */
export async function registerSw(): Promise<ServiceWorkerRegistration | null> {
  if (!swSupported() || !import.meta.env.PROD) return null;
  try {
    return await navigator.serviceWorker.register("./sw.js", { scope: "./" });
  } catch {
    return null;
  }
}

export async function pushState(): Promise<PushState> {
  if (!pushSupported()) return "unsupported";
  const perm = Notification.permission;
  if (perm === "denied") return "denied";
  if (perm !== "granted") return "default";
  try {
    const reg = await navigator.serviceWorker.getRegistration();
    const sub = await reg?.pushManager.getSubscription();
    return sub ? "subscribed" : "granted";
  } catch {
    return "granted";
  }
}

function b64ToBytes(b64: string): Uint8Array {
  const pad = "=".repeat((4 - (b64.length % 4)) % 4);
  const s = (b64 + pad).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(s);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/** Must be called from a user gesture (tap). Resolves the resulting state. */
export async function enablePush(): Promise<PushState> {
  if (!pushSupported()) return "unsupported";
  const perm = await Notification.requestPermission();
  if (perm !== "granted") return perm === "denied" ? "denied" : "default";
  const reg =
    (await navigator.serviceWorker.getRegistration()) ?? (await registerSw());
  if (!reg) return "granted";
  const vapid = await get<{ public_key?: string; publicKey?: string }>(
    "/api/v2/push/vapid",
  );
  const key = vapid?.public_key ?? vapid?.publicKey;
  if (!key) throw new Error("Server has no web-push key configured yet.");
  let sub = await reg.pushManager.getSubscription();
  if (!sub) {
    sub = await reg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: b64ToBytes(key) as BufferSource,
    });
  }
  await post("/api/v2/push/subscriptions", {
    subscription: sub.toJSON(),
    device_id: session.server().device_id,
    device_name: await deviceName(),
  });
  return "subscribed";
}

export async function disablePush(): Promise<void> {
  if (!pushSupported()) return;
  try {
    const reg = await navigator.serviceWorker.getRegistration();
    const sub = await reg?.pushManager.getSubscription();
    if (sub) {
      await sub.unsubscribe();
      await post("/api/v2/push/subscriptions/delete", {
        endpoint: sub.endpoint,
      }).catch(() => {});
    }
  } catch {
    /* ignore */
  }
}

/** Mirror the unread count on the home-screen icon where supported. */
export function setAppBadge(n: number) {
  try {
    const nav = navigator as Navigator & {
      setAppBadge?: (n?: number) => Promise<void>;
      clearAppBadge?: () => Promise<void>;
    };
    if (n > 0) nav.setAppBadge?.(n).catch(() => {});
    else nav.clearAppBadge?.().catch(() => {});
  } catch {
    /* ignore */
  }
}

/** Ask the browser not to evict our storage (token, drafts) under pressure. */
export function persistStorage() {
  try {
    navigator.storage?.persist?.().catch(() => {});
  } catch {
    /* ignore */
  }
}

/** iOS Safari, not yet installed to the home screen. */
export function isIosSafariBrowser(): boolean {
  const ua = navigator.userAgent;
  const ios = /iPhone|iPad|iPod/.test(ua);
  const standalone =
    (navigator as Navigator & { standalone?: boolean }).standalone === true ||
    matchMedia("(display-mode: standalone)").matches;
  return ios && !standalone && !window.CortexNative;
}
