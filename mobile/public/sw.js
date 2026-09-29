/* Cortex mobile service worker (hand-written; no build-time plugin).
 *
 * - App shell: network-first for navigations with a cached fallback, so a
 *   cold start with no signal still shows the app; cache-first for hashed
 *   /assets/*. Never caches /api or /ws.
 * - Push: shows the notification from the JSON payload
 *   { title, body, navigate, app_badge } and mirrors the badge count.
 * - Notification click: focuses an open client and asks it to navigate, or
 *   opens the app at the deep link (`/#<navigate>`).
 */
const CACHE = "cortex-shell-v1";
const SHELL = ["./", "./index.html", "./manifest.webmanifest"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL).catch(() => {}))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith("/api") || url.pathname.startsWith("/ws")) return;

  if (req.mode === "navigate") {
    event.respondWith(
      fetch(req)
        .then((res) => {
          const copy = res.clone();
          caches
            .open(CACHE)
            .then((c) => c.put("./index.html", copy))
            .catch(() => {});
          return res;
        })
        .catch(() =>
          caches.match("./index.html").then((r) => r || Response.error()),
        ),
    );
    return;
  }

  if (url.pathname.includes("/assets/")) {
    event.respondWith(
      caches.match(req).then(
        (hit) =>
          hit ||
          fetch(req).then((res) => {
            if (res.ok) {
              const copy = res.clone();
              caches
                .open(CACHE)
                .then((c) => c.put(req, copy))
                .catch(() => {});
            }
            return res;
          }),
      ),
    );
  }
});

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = { title: "Cortex", body: event.data ? event.data.text() : "" };
  }
  const title = data.title || "Cortex";
  const navigateTo =
    typeof data.navigate === "string" ? data.navigate : "/inbox";
  const opts = {
    body: data.body || "",
    icon: "./icon-192.png",
    badge: "./icon-192.png",
    tag: data.tag || navigateTo,
    renotify: !!data.tag,
    data: { navigate: navigateTo },
  };
  const work = [self.registration.showNotification(title, opts)];
  if (typeof data.app_badge === "number" && "setAppBadge" in self.navigator) {
    work.push(
      data.app_badge > 0
        ? self.navigator.setAppBadge(data.app_badge).catch(() => {})
        : self.navigator.clearAppBadge().catch(() => {}),
    );
  }
  event.waitUntil(Promise.all(work));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const path =
    (event.notification.data && event.notification.data.navigate) || "/inbox";
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((list) => {
        const client = list.find((c) => "focus" in c);
        if (client) {
          client.postMessage({ type: "navigate", path });
          return client.focus();
        }
        const base = new URL("./", self.location.href).href;
        return self.clients.openWindow(`${base}#${path}`);
      }),
  );
});
