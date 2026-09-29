# Cortex mobile client

The phone client for Cortex: a React + Vite single-page app served by the
desktop's embedded server (`src-tauri/src/mobile_server`) and wrapped by the
Capacitor shell in `native/` for the Android APK and iOS builds. Architecture,
pairing, install and push are documented in [docs/MOBILE.md](../docs/MOBILE.md).

## Layout

```
src/
  main.tsx, App.tsx        boot → pair | unreachable | ready; tabs; push prompt
  lib/api.ts               every /api/v2 endpoint with legacy /api/* fallbacks
  lib/ws.ts                one WebSocket: auth, subscribe, backoff, resync
  lib/store.tsx            connection state, capabilities, project, theme, badge
  lib/nav.ts               hash router (#/chats, #/threads/:id, #/inbox, …)
  lib/push.ts              service worker + VAPID subscription
  lib/session.ts           server URL + bearer token, pairing-link parsing
  lib/demo.ts              in-memory server for the demo mode
  views/                   Pair, Chats, Thread, Inbox, Projects, Runs, More
  components/              Shell, Composer, ToolCard, ApprovalCard, Gauge, ui
public/manifest.webmanifest, public/sw.js
```

## Build

```sh
pnpm build:mobile     # from the repo root → mobile/dist (bundled as a Tauri resource)
```

`dist/` is git-ignored. `tauri dev` and `tauri build` run this step; CI runs it
before `cargo` so the resource exists.

## Dev against a running Cortex

```sh
cd mobile
VITE_API_BASE=http://localhost:8788 pnpm dev   # proxies /api and /ws
```

Open the printed LAN URL on the phone. Requests from a non-loopback address
need a paired token, so pair from **Settings → Mobile** on the desktop first,
or open the demo from the pairing screen for layout work with no server.

## Modes

Boot probes `GET /api/v2/capabilities`:

- 200 → v2 mode; features come from `capabilities.features`.
- 404 → legacy mode against an older Cortex (`/api/sessions`, `/api/chat`);
  runs, routines, git and devices show a "needs a newer Cortex" state.
- 401 → pairing screen. Any later 401 returns there.

## Native shell

The web app never imports Capacitor. `native/bridge.ts` installs
`window.CortexNative` (haptics, QR scan, secure storage, deep links, share)
before the bundle runs; every member is optional and feature-detected. Deep
links arrive as a `cortex:deeplink` event carrying the URL.
