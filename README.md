# Cortex

> Central-brain desktop app — one chat surface for every coding agent.

Cortex is NetworkBound's command-and-control desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, Gemini, Ollama, future) from a single chat. A built-in **Cortex Gateway orchestrator** decides which agent handles each task — or fans out to several agents on the same project. Zed-inspired UX, Sentry-style observability for agent runs and homelab health.

**Status:** Phase 0 scaffold. Not yet runnable end-to-end. See `docs/ARCHITECTURE.md` and `docs/adrs/` for the plan.

## What it is

- **Frontend:** React + TypeScript + Vite, Zed-style command palette and panes.
- **Backend:** Rust via Tauri 2 — agent process supervision, memory I/O, OS keychain, OpenAI-compatible client for the Cortex Gateway, OpenTelemetry-style tracing.
- **Agents as adapters:** every agent (Claude CLI, Codex CLI, Cortex Gateway backend, local Ollama) is a one-file plugin behind a small `AgentAdapter` trait. Add a new agent by dropping a file in `src-tauri/src/agents/`.
- **Memory integration:** reads/writes NetworkBound's existing `~/.claude/projects/*/memory/`, `~/.claude-mem/chroma`, and project `runbooks/` directories. Optional Obsidian vault path.
- **Observability:** internal dashboard (token use, latency, error rates), distributed traces for multi-agent fan-outs, Sentry SDK for app crash reporting, homelab service health (LXC 153/154, Host A/B).
- **Cross-platform:** Windows `.msi`, Linux `.deb` / `.AppImage`, macOS `.dmg` — built via GitHub Actions matrix.

## Quick start (dev)

```bash
# Linux build deps (one-time, requires sudo)
sudo apt install -y libwebkit2gtk-4.1-dev libssl-dev libayatana-appindicator3-dev \
                    librsvg2-dev build-essential curl wget file libxdo-dev \
                    libdbus-1-dev pkg-config

# Install JS deps and dev-run
pnpm install
pnpm tauri dev
```

First-run flow prompts for the Cortex Gateway backend URL (e.g. `http://gateway.example:8642`) and API key (stored in OS keychain, never on disk).

## Building on Linux

On modern Linux (Fedora 40+ / recent glibc, where binaries carry a `.relr.dyn` section), build with:

```bash
NO_STRIP=true pnpm tauri build   # or: pnpm tauri:build:linux
```

This stops Tauri's bundled `linuxdeploy` from running `strip`, which can't parse the
`.relr.dyn` section and otherwise aborts the AppImage step. The `.deb` and `.rpm`
bundles build fine without it; the flag is only needed so the AppImage is also produced.

## Building on Windows

Build natively on Windows 10/11 (x64). One-time prerequisites:

- **Visual Studio Build Tools** with the **Desktop development with C++** workload
  (MSVC v143 + Windows SDK) — https://visualstudio.microsoft.com/downloads/
- **Rust** via rustup (uses the MSVC toolchain) — https://rustup.rs
- **Node.js 22 LTS** — https://nodejs.org — then `corepack enable pnpm`
- **WebView2 Runtime** — preinstalled on Win11/most Win10; otherwise install the
  Evergreen bootstrapper — https://developer.microsoft.com/microsoft-edge/webview2/
- NSIS (`.exe`) and WiX (`.msi`) are downloaded automatically by Tauri on first build.

Then, in PowerShell:

```powershell
pnpm install
pnpm tauri build
```

Output under `src-tauri\target\release\bundle\`:

- `nsis\Cortex_<version>_x64-setup.exe` — recommended (per-user install, no admin)
- `msi\Cortex_<version>_x64_en-US.msi` — MSI alternative

The build is unsigned, so SmartScreen warns on first run (More info → Run anyway);
add a code-signing certificate to remove that.

## Repo layout

```
cortex/
├── docs/                  Architecture, ADRs, security, research
├── src/                   React frontend (Vite)
├── src-tauri/             Rust backend
│   ├── src/agents/        Agent adapters (one file per agent)
│   ├── src/memory/        Memory readers/writers
│   ├── src/gateway/       OpenAI-compatible client for the backend gateway
│   ├── src/observability/ Tracing, telemetry, Sentry integration
│   └── src/commands/      Tauri IPC commands exposed to renderer
├── .github/workflows/     CI: cross-platform builds + release
└── scripts/               Dev/build helpers
```

## Adding a new agent

See `docs/CONTRIBUTING.md`. Short version: implement `AgentAdapter` in `src-tauri/src/agents/your_agent.rs`, register it in `registry.rs`, and add UI metadata in `src/lib/agents.ts`. ~60 lines total.

## Docs

- [`ARCHITECTURE.md`](docs/ARCHITECTURE.md) — module boundaries, data flow, IPC contract
- [`SECURITY.md`](docs/SECURITY.md) — threat model, secret handling
- [`PRIVACY.md`](docs/PRIVACY.md) — what leaves the device, what stays local
- [`adrs/`](docs/adrs/) — architectural decisions
- [`research/RESEARCH-FINDINGS.md`](docs/research/RESEARCH-FINDINGS.md) — Zed, Sentry, Tauri, multi-agent orchestration research that informed the design

## License

TBD — pick before first public release.
