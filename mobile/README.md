# Cortex Mobile

A Claude-app-like mobile web client for Cortex. Mobile-first, installable PWA,
dark amber-on-black theme matching the desktop app.

## How it's served

This is its **own** pnpm package (not linked to the repo root). The Cortex
embedded server (`src-tauri/src/mobile_server/`) serves the built SPA from
`mobile/dist` **at the same origin as the API**. So:

- All fetches are **relative** (`/api/...`).
- The WebSocket is `new WebSocket(\`ws(s)://\${location.host}/ws\`)`.

Because the SPA and API share an origin, there is no CORS/base-URL config in
production — `vite.config.ts` sets `base: './'` so the built asset URLs are
relative and survive being served from any mount point.

> **A build step is required before serving.** The server resolves `mobile/dist`
> at runtime (`router.rs::mobile_dist_dir`, overridable via `CORTEX_MOBILE_DIST`).
> `dist` is gitignored — run `pnpm build` (in release/CI) before the server can
> serve the app. If `dist` is missing, the API and WS keep working; only the
> static SPA 404s.

## Build

```sh
cd mobile
pnpm install
pnpm build      # tsc -b && vite build  →  mobile/dist
```

## Dev against a live Cortex

`pnpm dev` runs Vite's dev server. Point it at a running Cortex with
`VITE_API_BASE` — Vite then proxies both `/api` and `/ws` (WebSocket upgrade
included) to that origin, so the SPA keeps using same-origin relative paths:

```sh
# headless server default is loopback :5000 on the box running it
VITE_API_BASE=http://localhost:8788 pnpm dev
# then open the printed http://<lan-ip>:5173 on your phone
```

Without `VITE_API_BASE` the dev server has no API to talk to (all calls 404) —
useful only for pure layout work.

## API contract

Same-origin endpoints consumed (see `src/lib/api.ts`):

- `GET /api/health` → `{ ok, version }`
- `GET /api/projects` → project objects (`name`, `root`, `group`, …; rendered defensively)
- `GET /api/models` → `string[]`
- `POST /api/chat` → `202 { run_id, session_id }`, output streams over WS
- `POST /api/ultimate` → `200 { run_id, result }`, progress also streams over WS
- `GET /api/approvals` / `POST /api/approvals/{id}`
- `GET /ws` → one shared WebSocket; frames routed by `run_id` (see `src/lib/ws.ts`)

## Screens

- **Chat** — model picker, streaming markdown replies, tool-call chips.
- **Ultimate** — goal + fan-out + lead model; live timeline (plan → models
  racing per subtask → merge → synthesis → cost).
- **Projects** — tap to set the active `project_root` used by Chat + Ultimate.
- **Inbox** — pending approvals; Approve/Reject (no optimistic UI — a row only
  clears after the request succeeds). Polls every 4s and reacts to WS frames.

## Known rough edges

- The ultimate `run_id` is only returned when the (blocking) POST resolves. To
  show the live timeline we pin onto the first incoming `ultimate*` WS frame's
  `run_id` while a run is in flight. With multiple concurrent ultimate runs from
  different clients this could mis-attribute early frames; for a single-user
  phone client it's fine.
- Approvals: the server records/acknowledges decisions but (per its own handler
  docs) does not yet re-inject a manual decision into an in-flight adapter run.
  The UI resolves the pending item correctly regardless.
