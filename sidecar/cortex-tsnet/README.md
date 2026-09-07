# cortex-tsnet

A userspace [Tailscale](https://tailscale.com) sidecar for Cortex, built on
[`tailscale.com/tsnet`](https://pkg.go.dev/tailscale.com/tsnet). It joins the
tailnet **without root, without a system daemon, and without admin** — the node
lives entirely inside this process — and exposes a local **SOCKS5 proxy** whose
dialer routes every connection over the tailnet.

Cortex routes its home-service HTTP traffic (the Cortex Gateway and Ollama)
through this proxy so requests to tailnet hosts resolve via MagicDNS and tunnel
over WireGuard, with no changes to the host's networking.

## Build

```sh
cd sidecar/cortex-tsnet
go build -o cortex-tsnet .
```

Go 1.26+. The binary is large (~30 MB) — that's expected; tsnet statically links
a full WireGuard + gVisor userspace netstack.

## Run

```sh
./cortex-tsnet \
  --hostname cortex \
  --state-dir ~/.config/cortex/tsnet/cortex \
  --socks 127.0.0.1:1055
# optionally, headless login with a pre-auth key:
TS_AUTHKEY=tskey-auth-... ./cortex-tsnet ...
```

### Flags / env

| Flag          | Env          | Default                         | Meaning |
|---------------|--------------|---------------------------------|---------|
| `--hostname`  |              | `cortex`                        | Tailnet hostname for this node. |
| `--state-dir` |              | per-user config dir (`<UserConfigDir>/cortex/tsnet/<hostname>`) | Where tsnet persists node identity/keys. Reused across runs so re-auth isn't needed. |
| `--authkey`   | `TS_AUTHKEY` | *(empty)*                       | Tailnet pre-auth key. Optional. Enables headless (non-interactive) login. **Never logged or echoed.** |
| `--socks`     |              | `127.0.0.1:1055`                | Local SOCKS5 listen address. |

If neither an auth key nor saved state is present, tsnet performs interactive
login: the sidecar emits a `needs-login` status line with the login URL (see
below). Open it in a browser to authorize the node; the sidecar then proceeds to
`connected` automatically.

## Status protocol (stdout)

The sidecar writes **one JSON object per line to stdout**. Cortex parses these.
All human/debug logging (including tsnet's own logs) goes to **stderr**, so
stdout is a clean status channel.

| Line | When |
|------|------|
| `{"state":"starting"}` | Process came up, before the node is authenticated. |
| `{"state":"needs-login","url":"https://login.tailscale.com/..."}` | Interactive auth required. May be emitted more than once; the URL is stable. |
| `{"state":"connected","ip":"100.x.x.x","dnsname":"cortex.<tailnet>.ts.net"}` | Node is up on the tailnet. `ip` is the tailnet IPv4; `dnsname` is the MagicDNS name. |
| `{"state":"error","msg":"..."}` | Fatal startup/runtime error. The process then exits non-zero. |

The login URL is captured from two sources for robustness: scraping tsnet's log
lines, and polling `LocalClient().Status().AuthURL`.

## SOCKS5 behavior

The proxy's dialer is the tsnet node's `Dial`, so connections egress from the
tailnet. Name resolution is deferred to the tailnet node (MagicDNS resolves
tailnet-side) — clients should use **`socks5h://`** (resolve-at-proxy) so DNS
never touches the local box. Cortex builds its reqwest clients with
`socks5h://127.0.0.1:1055` when embedded Tailscale is enabled + connected.

## Notes

- `Ephemeral: false` — the node persists in the tailnet between runs (reuses
  `--state-dir`), so you authorize once.
## Bundling with the desktop app (Tauri sidecar)

The desktop app ships this binary as a Tauri **sidecar** (`externalBin`). Tauri
resolves sidecars by `<name>-<target-triple>` (e.g.
`cortex-tsnet-x86_64-unknown-linux-gnu`, `cortex-tsnet-aarch64-apple-darwin`,
`cortex-tsnet-x86_64-pc-windows-msvc.exe`) and strips the triple when it lands
next to the app binary as `cortex-tsnet`.

**Why this is NOT wired into `src-tauri/tauri.conf.json` yet:** `tauri-build`
copies external binaries at *build-script* time — `cargo check`/`cargo build`
**fails** if the `<name>-<triple>` file for the current target is missing. Since
the per-platform sidecars are produced in CI (a follow-up), committing an
`externalBin` entry now would break every clean build that hasn't pre-built the
sidecar. So the entry is documented here and added by CI.

CI must, per target triple:

1. `cd sidecar/cortex-tsnet && GOOS/GOARCH set for the triple && go build -o cortex-tsnet-<triple>[.exe] .`
2. Place the triple-named binary where `externalBin` points (e.g. `src-tauri/binaries/`).
3. Inject into `src-tauri/tauri.conf.json` under `bundle`:

   ```json
   "externalBin": ["binaries/cortex-tsnet"]
   ```

   (Tauri appends `-<triple>` itself; provide one file per supported triple.)

See the integration notes in `src-tauri/src/tailscale/`.
