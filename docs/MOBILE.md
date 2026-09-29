# Cortex on a phone

The iOS and Android apps are the mobile web client in `mobile/` wrapped in a
[Capacitor](https://capacitorjs.com) shell (`mobile/native/`). The phone does no
agent work itself: it is a full client of the Cortex backend running on your
desktop (the Tauri app) or on a headless `cortex-serve` box, reached over
Tailscale or your LAN. Everything the desktop can do through the API (threads,
streaming chat, approvals, projects, git, checkpoints, replay, routines,
reliability, usage) the phone does through the same routes, so you get one
brain, one history, one set of policies.

```
phone (Capacitor + React SPA)  ──http(s) + ws──▶  Cortex desktop / cortex-serve
        cortex://pair?...                          :8788 (loopback by default)
        Bearer token per device                    `tailscale serve` or LAN bind
```

## How it works

| Piece          | Where                              | Notes                                                                                                                                                                           |
| -------------- | ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Web client     | `mobile/src`                       | React + Vite; built by `pnpm build:mobile` into `mobile/dist`. Also served as a PWA by the desktop.                                                                             |
| Native shell   | `mobile/native`                    | Capacitor 8 config + plugins. Own `package.json`, no lockfile (CI does `npm install`).                                                                                          |
| Bridge         | `mobile/native/bridge.ts`          | Bundled to `mobile/dist/native-bridge.js` and injected into `index.html` by `scripts/mobile/build-bridge.mjs`. Installs `window.CortexNative` (see below). Absent in a browser. |
| Native patches | `scripts/mobile/patch-native.sh`   | Deep-link scheme, LAN cleartext, usage strings, versions, privacy manifest — applied after `cap add` because `android/` and `ios/` are generated and gitignored.                |
| Icons / splash | `scripts/mobile/prepare-assets.sh` | `@capacitor/assets` from `src-tauri/icons/source.png`.                                                                                                                          |
| CI             | `.github/workflows/mobile.yml`     | Builds APK + iOS simulator bundle on every push; attaches APKs to tag releases.                                                                                                 |
| Server         | `src-tauri/src/mobile_server`      | The axum router; the `/api/v2/*` routes and bearer auth are the mobile contract.                                                                                                |

### `window.CortexNative`

The web app never imports Capacitor. On a phone the bridge script sets:

```ts
interface CortexNative {
  platform: "ios" | "android";
  version: string;
  haptic(
    kind:
      | "light"
      | "medium"
      | "heavy"
      | "success"
      | "warning"
      | "error"
      | "selection",
  ): Promise<void>;
  scanQr(): Promise<string | null>; // camera QR → raw text, null if cancelled
  secureGet(key: string): Promise<string | null>;
  secureSet(key: string, value: string): Promise<void>;
  secureDelete(key: string): Promise<void>;
  openExternal(url: string): Promise<void>; // system browser sheet
  share(text: string, title?: string): Promise<void>;
  setStatusBar(style: "light" | "dark"): Promise<void>;
  // extras
  onDeepLink(cb: (url: string) => void): () => void;
  getLaunchUrl(): Promise<string | null>;
  hideSplash(): Promise<void>;
  hideKeyboard(): Promise<void>;
}
```

`secure*` is backed by `@capacitor/preferences` (iOS UserDefaults / Android
SharedPreferences): app-sandboxed, but not encrypted at rest. Swapping in a
keychain/keystore plugin only changes those three functions in `bridge.ts`.

Deep links are also re-broadcast as a `cortex:deeplink` `CustomEvent<string>`
on `window` (`event.detail` is the URL; the cold-start URL is delivered the
same way just after `load`). `cortex:resume` / `cortex:pause` fire on
foreground/background so the client can reconnect its WebSocket.

## Pairing

1. Desktop: **Settings → Mobile** shows a QR code and a 6-digit code (valid
   10 minutes). The QR encodes `cortex://pair?url=<server base>&code=<digits>`.
2. Phone: open Cortex → **Pair** → scan the QR (or type the URL + code). The
   app calls `POST /api/v2/pair` and stores the returned bearer token with
   `secureSet`.
3. From then on every request carries `Authorization: Bearer …`; the desktop
   lists and revokes devices under **Settings → Mobile → Devices**
   (`GET/DELETE /api/v2/devices`). Loopback requests stay unauthenticated, so
   the PWA opened on the desktop itself keeps working.

The server base URL is whatever the desktop is reachable at from the phone:
`https://<machine>.<tailnet>.ts.net` with `tailscale serve`, or
`http://100.x.y.z:8788` / `http://192.168.x.y:8788` if you bind the mobile
server beyond loopback (`CORTEX_MOBILE_BIND`, see `docs/SECURITY.md`).

## Installing

### Android (APK)

Every CI run of the **Mobile** workflow uploads `Cortex-<version>-android-debug.apk`
as an artifact; tagged releases attach it to the GitHub release. It is signed
with the SDK debug key, which is enough to sideload:

1. Download the APK on the phone (or `adb install Cortex-*.apk`).
2. Allow "install unknown apps" for your browser/file manager when prompted.
3. Open Cortex, pair.

For automatic updates install [Obtainium](https://github.com/ImranR98/Obtainium)
and add `https://github.com/NetworkBound/cortex`; it watches the releases page
and installs new APKs as they appear.

Debug-key APKs from different machines/CI runs all use the same well-known key,
so updates install over each other. A Play-store-signed release APK is built
when these repository secrets exist:

| Secret                      | Value                                                      |
| --------------------------- | ---------------------------------------------------------- |
| `ANDROID_KEYSTORE_BASE64`   | `base64 -w0 release.jks`                                   |
| `ANDROID_KEYSTORE_PASSWORD` | keystore password                                          |
| `ANDROID_KEY_ALIAS`         | key alias                                                  |
| `ANDROID_KEY_PASSWORD`      | key password (optional; defaults to the keystore password) |

### iOS (free: installable web app)

Apple does not allow sideloading, and a native build needs the paid developer
program to leave a Mac. The free path is the same client installed as a Home
Screen web app, which on iOS 16.4+ gets full screen, an icon, Web Push and
the app badge:

1. On the desktop, expose the mobile server over HTTPS with Tailscale:
   `tailscale serve --bg 8788` (free Personal plan; Tailscale issues the
   Let's Encrypt certificate for `https://<machine>.<tailnet>.ts.net`).
   Install Tailscale on the phone and sign in to the same tailnet.
2. Open **Settings → Mobile** on the desktop and scan the QR with the iPhone
   camera, or open `https://<machine>.<tailnet>.ts.net/#/pair?code=<digits>`
   in Safari. Enter the 6-digit code if asked.
3. In Safari tap **Share → Add to Home Screen → Add**, then open Cortex from
   the Home Screen and allow notifications when prompted. Cortex remembers
   the pairing token per installed app.

Notifications arrive through Apple's Web Push service; the server sends them
directly (RFC 8291/8292, VAPID key kept in the vault), so there is no third
party in the path. Safari in a tab does not receive Web Push; the Home Screen
install does.

### iOS (TestFlight, optional)

With an Apple Developer Program membership the **Mobile** workflow also ships
the Capacitor build. Without secrets it only proves the Xcode project compiles
(simulator build, uploaded as `Cortex-<version>-ios-simulator.app.zip`; drag it
onto a booted Simulator to try it). With these secrets the same job archives,
signs, exports an `.ipa` and uploads it to App Store Connect, where it shows up
in TestFlight a few minutes later:

| Secret                                                     | Value                                                        |
| ---------------------------------------------------------- | ------------------------------------------------------------ |
| `IOS_P12_BASE64` / `IOS_P12_PASSWORD`                      | Apple Distribution certificate + key, exported as .p12       |
| `IOS_PROVISIONING_PROFILE_BASE64`                          | App Store provisioning profile for `com.networkbound.cortex` |
| `APPLE_TEAM_ID`                                            | 10-character team ID                                         |
| `APP_STORE_CONNECT_KEY_ID` / `APP_STORE_CONNECT_ISSUER_ID` | App Store Connect API key (App Manager role)                 |
| `APP_STORE_CONNECT_API_KEY_BASE64`                         | the key's `.p8`                                              |

The bundle id `com.networkbound.cortex` must exist in the developer portal
first. `MARKETING_VERSION` is the Cortex version; `CURRENT_PROJECT_VERSION`
(build number) is derived from it (`3.5.0 → 30500`) so every release uploads
a higher build.

## Deep links

The app registers the `cortex://` scheme on both platforms:

| URL                          | Action                                                        |
| ---------------------------- | ------------------------------------------------------------- |
| `cortex://pair?url=…&code=…` | Start pairing with that server (what the desktop QR encodes). |
| `cortex://inbox`             | Open the approvals inbox.                                     |
| `cortex://threads/<id>`      | Open a thread.                                                |
| `cortex://approvals/<id>`    | Open one approval.                                            |

Cold-start links come from `getLaunchUrl()`, links while running from
`onDeepLink` / the `cortex:deeplink` event. Both platforms treat custom schemes
as unverified, so any app can register `cortex://`; never put a bearer token in
a link — the pairing code is one-time and expires.

## Push

Two paths, both free and both sent by the desktop itself:

- **Web Push** (installed web app on iPhone or Android, Chrome/Firefox/Safari):
  the app subscribes through its service worker; the server encrypts each
  message (RFC 8291) and signs it (RFC 8292 VAPID) and posts it straight to
  the browser vendor's push service. Egress is limited to those services.
  Events: approval needed, run finished, run failed, quota above 90 %. Tapping
  opens the approval, thread or inbox.
- **ntfy / Gotify** (**Settings → Notifications**): the existing notifier, for
  the native Android build or any phone with the ntfy app. Its click URL opens
  the web client; `GET /api/v2/push/status` tells the app what is configured.

## Limitations

- **No APNs/FCM in the native builds.** The Capacitor apps get approval
  prompts over the WebSocket while open, or via ntfy; Web Push covers the
  installed web app. A native push relay is a later step.

## Local development

Prerequisites: Node 22 + pnpm, JDK 21 + Android Studio (SDK 36) for Android,
Xcode 26 for iOS (macOS only).

```bash
pnpm install && pnpm build:mobile          # web app → mobile/dist
cd mobile/native
npm install                                # Capacitor + plugins (no lockfile)
npm run bridge                             # mobile/dist/native-bridge.js + <script> tag
npm run add:android                        # cap add android + patch-native.sh
bash ../../scripts/mobile/prepare-assets.sh --android   # optional icons/splash
npx cap sync android
npx cap run android                        # pick a device/emulator
# iOS on macOS:
npm run add:ios && npx cap sync ios && npx cap run ios
```

Iterating on the web app: rebuild (`pnpm build:mobile`), re-run
`npm run bridge`, then `npx cap sync` (or `cap copy`) and relaunch. For a
hot-reload loop point Capacitor at the Vite dev server: add
`server: { url: "http://<your-lan-ip>:5173", cleartext: true }` to
`capacitor.config.ts` temporarily and run `pnpm --dir mobile dev` with
`VITE_API_BASE` pointing at a Cortex; the bridge script is not injected in
that mode, so native-only features fall back to their web paths.

`npx cap add` refuses to run when `android/` or `ios/` already exists; delete
the directory to regenerate (it is gitignored, and everything Cortex-specific
is reapplied by `patch-native.sh`).
