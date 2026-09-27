# Cortex Architecture

## One-line goal

A single desktop window where the user sends a task, Cortex picks the agent (or agents) that can do it, those agents execute against the right project, and every step is recorded locally so it can be replayed and measured later.

## Big picture

```
┌────────────────────────────── Cortex desktop app (Tauri 2) ──────────────────────────────┐
│                                                                                          │
│  WebView (React + TS, src/)                                                              │
│  ┌──────────────┐ ┌──────────────────────────┐ ┌─────────────────┐ ┌──────────────────┐  │
│  │ Project /    │ │ Chat pane + composer     │ │ Agent sidebar / │ │ Observability:   │  │
│  │ file sidebar │ │ (streams agent events)   │ │ model picker    │ │ replay, metrics  │  │
│  └──────┬───────┘ └────────────┬─────────────┘ └────────┬────────┘ └────────┬─────────┘  │
│         └──────── invoke() ────┴──── Tauri IPC ─────────┴─── listen() ──────┘            │
│                                          │                                               │
│  Rust backend (src-tauri/src/, crate `cortex_lib`)                                       │
│  ┌───────────────────────────────────────▼──────────────────────────────────────────┐    │
│  │ commands/*  — the only surface the renderer can call (generate_handler!)         │    │
│  ├──────────────┬──────────────┬───────────────┬──────────────┬─────────────────────┤    │
│  │ orchestrator │ agents       │ memory /      │ observability│ mobile_server       │    │
│  │ routing,     │ adapters +   │ brain /       │ SQLite spans,│ axum HTTP + WS on   │    │
│  │ sandbox,     │ registry     │ retrieval     │ audit, health│ 127.0.0.1:8788      │    │
│  │ policies     │              │               │              │ serves mobile/dist  │    │
│  └──────┬───────┴──────┬───────┴───────┬───────┴──────┬───────┴──────────┬──────────┘    │
└─────────┼──────────────┼───────────────┼──────────────┼──────────────────┼───────────────┘
          │              │               │              │                  │
   CLI subprocesses   HTTP(S)        ~/.claude/*,    data_local_dir/    cortex-tsnet sidecar
   claude, codex,     OpenAI-compat  Obsidian vault  cortex/            (Go, tsnet SOCKS5)
   gemini, qwen, …    APIs, Ollama,  (indexed, not   cortex-local.db    → phone/tablet over
   (own logins)       gateway        duplicated)     + audit.log        the tailnet
```

Three processes can be involved:

- **`cortex`** — the desktop app. Rust backend + WebView.
- **`cortex-tsnet`** — optional Go sidecar (`sidecar/cortex-tsnet`), a userspace Tailscale node exposing a local SOCKS5 proxy. Bundled via `bundle.externalBin`, spawned by `tailscale::manager` only when embedded Tailscale is enabled and no system Tailscale is present. On Windows it can instead bridge to a Tailscale running inside WSL (`tailscale::wsl`, `remote_forward`).
- **`cortex-serve`** — headless build of the same backend (`src-tauri/src/bin/cortex-serve.rs`) that runs only the mobile HTTP/WS server, for a VM behind `tailscale serve`.

## Module boundaries

### Frontend — `src/` (React 18 + TypeScript, Vite)

| Path | Responsibility |
|---|---|
| `App.tsx` | Top-level layout, activity bar, panel routing, boot-time hooks (theme, updater, onboarding) |
| `components/` | One file per panel or modal (~150). `ChatPane`, `ComposerPanel`, `ModelPicker`, `ObservabilityPanel`, `RunReplayView`, `ReliabilityDashboard`, `SettingsModal`, `TerminalPane`, `FileExplorer`, … |
| `lib/` | One module per backend feature. Each wraps the matching `invoke("<command>")` calls and event subscriptions with typed functions (`lib/cortex-bridge.ts` for chat, events and Tailscale, `lib/updater.ts`, `lib/self-update.ts`, `lib/checkpoints.ts`, …). Components call these, not `invoke` directly. |
| `state/` | zustand stores (`store.ts` for app/session state, `threads.ts`, `jobs.ts`) |
| `styles/` | CSS; themes are CSS variables switched at runtime |

UI preferences (theme, last project, update URL, panel layout) live in the WebView's `localStorage`. Anything the backend needs is persisted by the backend, not the renderer.

### Backend — `src-tauri/src/` (Rust, lib crate `cortex_lib`)

| Module | Responsibility |
|---|---|
| `lib.rs` | App bootstrap: tracing, keychain seeding, Tailscale mode selection, `AppState`, agent registry seeding, plugin registration, `generate_handler!` (~430 commands), `build_headless_state` for `cortex-serve` |
| `commands/` (138 modules) | Tauri IPC commands. Thin: parse args, call into the modules below, map errors to `String`. `commands/chat.rs` is the big one — it runs the tool loop, sandbox/guardrail/approval gates and event fan-out |
| `agents/` | `adapter.rs` (`AgentAdapter` trait, `ChatRequest`, `AgentEvent`), `registry.rs`, and the adapters: `local_cli.rs` (`GenericCliAgent` driven by per-CLI `*_spec.rs` files — Claude, Codex, Gemini, Qwen, Grok, aider, Mistral Vibe), `openai_compat.rs` (`PROVIDERS` table), `local_runtime.rs` (`RUNTIMES` table), `custom_endpoint.rs` (Model Fabric), `ollama.rs`, `gateway_remote.rs`, `anthropic_direct.rs`/`openai_direct.rs` behind the `standalone` feature. `cli_discovery.rs` finds CLIs including Windows `.cmd`/`.exe` shims |
| `orchestrator/` | Routing (`cost_router.rs`, `aliases.rs`, `reasoning.rs`), multi-agent (`teams.rs`, `team_run.rs`, `ultimate.rs`, `architect.rs`), and the safety gates: `sandbox.rs` (three tiers), `guardrails.rs`, `command_policy.rs`, `safe_commands.rs`, `approvals.rs` / `approval_policy.rs` / `auto_approve.rs`, `trust.rs`, `profiles.rs` |
| `memory/`, `brain.rs`, `retrieval/` | Markdown/frontmatter parsing, Obsidian (files + REST), chat-history indexing, local embeddings (`embed.rs`), dedup, snapshots, optional sync. Cortex indexes the user's existing memory; it does not copy it |
| `observability/` | `tracing_store.rs` (SQLite: spans, events, chat turns, health, audit, embeddings, replay + reliability queries), `audit.rs` (append-only log), `crash.rs`, `homelab.rs` (health pollers), `webhooks.rs`, `sentry.rs` (opt-in, off by default; redaction helper) |
| `mobile_server/` | axum router, auth, WebSocket event stream, static `mobile/dist`. Bound to `127.0.0.1:8788` (`CORTEX_MOBILE_PORT`) |
| `tailscale/`, `remote_forward.rs` | Sidecar lifecycle, status protocol, WSL-Tailscale bridge, `maybe_tailscale_proxy` for the gateway/Ollama HTTP clients |
| `mcp/`, `skills/`, `hooks/`, `prp/`, `agui/` | MCP client + catalog, skill loader, lifecycle hooks, PRP runner, AG-UI translation |
| `git/`, `worktrees.rs`, `lanes.rs`, `projects/`, `repo_map.rs`, `watch_mode.rs`, `monitors.rs` | Project discovery (incl. native Windows paths and WSL UNC de-duplication), worktree lanes, repo maps, file watching |
| `terminal/`, `preview.rs`, `websearch.rs`, `usage.rs`, `pricing.rs`, `history_sync/`, `chat_import/` | PTY terminal (`portable-pty`), preview server, web search, usage/cost, provider history import |
| `infra_config.rs`, `paths.rs`, `app_state.rs`, `redact.rs`, `sys.rs`, `connectivity.rs` | `~/.cortex/infra.json` + env overrides, home-dir resolution (test-redirectable), config + keychain, secret scrubbing, platform helpers |

Registered Tauri plugins: `shell`, `fs`, `dialog`, `os`, `store`. Capabilities are in `src-tauri/capabilities/default.json` and are deliberately narrow: the renderer gets no secret access and only a few `fs:scope` paths; everything else is mediated by commands.

Cargo features: `standalone` (compile the direct Anthropic/OpenAI adapters), `remote_client` (thin "Cortex Home" window that shows a remote `cortex-serve` over the tailnet). Both off by default.

## Data flow — one message

1. User types in the composer and sends.
2. Frontend calls `invoke("chat_send", { sessionId, message, agent, projectRoot, … })` via `lib/cortex-bridge.ts`.
3. `commands::chat::chat_send` resolves the agent: explicit pick, alias, or `orchestrator::route` (capability filter, then cost, then local-first tie-break). The chosen reason is recorded.
4. A `TracingStore` span is opened (`start_agent_run`) and the adapter's `run(req, tx)` is spawned.
5. Every `AgentEvent` (`Started`, `Token`, `Reasoning`, `ToolCall`, `ToolResult`, `FileEdit`, `ApprovalRequest`, `Error`, `Done`) goes to three places: `emit("agent-event:{session_id}")` for the UI, `record_event` in SQLite, and — for tool calls — the gate chain (sandbox tier → guardrails → command policy → approval) which may block or pause the run.
6. `ChatPane` listens on `agent-event:{session_id}` and renders the stream; Run Replay and the Reliability Dashboard read the same span later.

Multi-agent modes (Teams, Lanes, Arena, Ultimate) fan out from step 3 and record one parent span with a child per agent.

## IPC contract

The renderer only calls commands listed in `generate_handler!` in `lib.rs`; each `src/lib/*.ts` module is the typed client for one feature area. Backend → frontend events are `emit`ted with a session-scoped name (`agent-event:{session_id}`), plus a small set of global events (approval prompts, Tailscale status, update notices). Grep `emit(` in `src-tauri/src` and `listen(` in `src/lib` for the current list.

## Storage

| What | Where | Format |
|---|---|---|
| Chat history, spans, run replay, reliability data, chat embeddings, health | `dirs::data_local_dir()/cortex/cortex-local.db` — Linux `~/.local/share/cortex/`, Windows `%LOCALAPPDATA%\cortex\`, macOS `~/Library/Application Support/cortex/` | SQLite (`observability/schema.sql` + migrations) |
| Audit log | same directory, `audit.log` | append-only text |
| Per-user config: infra endpoints, registered projects, teams, roles, skills, command/approval policies, trusted paths, budgets | `~/.cortex/` (`infra.json`, `registered-projects.json`, `teams/`, `skills/`, `command-policy.toml`, `trusted-paths.json`, …). `~` is `paths::home_dir()` — `%USERPROFILE%` on Windows | JSON / TOML |
| Per-project config | `<project>/.cortex/` (`sandbox.toml`, `approvals.toml`, `approval-policy.toml`, `command-policy.toml`, `danger.toml`, `rules/`, `profiles/`, `monitors/`) and `<project>/.cortexignore` | TOML / JSON |
| Secrets (API keys, gateway key, Tailscale auth key, vault master key) | OS keychain via `keyring` (Secret Service / macOS Keychain / Windows Credential Manager). Never on disk in clear | — |
| Tailscale node state | `<config dir>/cortex/tsnet/<hostname>` (sidecar) | tsnet state |
| Memory sources | `~/.claude/projects/*/memory/`, Obsidian vault, runbook dirs — read and indexed, not duplicated | external |
| UI preferences | WebView `localStorage` | — |

## Update mechanism

Cortex does not use `tauri-plugin-updater`. Two pieces exist:

- `commands/updater.rs` — `check_updates(manifest_url)`: fetches either a `{version, notes, url}` manifest or a Gitea/GitHub releases API array and compares against `CARGO_PKG_VERSION`. Shows an "update available" pill; never downloads. The URL is per-machine (`localStorage` `cortex.updateUrl`), there is no baked-in default, and cleartext HTTP is only allowed for loopback/private hosts.
- `commands/selfupdate.rs` — Linux AppImage only (`$APPIMAGE` set): downloads the newer AppImage from the configured Gitea release (`~/.cortex/infra.json` `update_gitea_host`), verifies a detached ed25519 signature against a baked-in public key (overridable via `update_pubkey` / `CORTEX_UPDATE_PUBKEY`), checks the ELF magic, and atomically renames over the running AppImage. Applies on next launch. On Windows, `.deb`, and dev builds it reports `supported: false`.

## What's NOT in scope (deliberately)

- Hosting models locally — that's a separate inference host's job.
- Replacing a gateway — when one is configured Cortex is a client of it; when not, the local CLIs work on their own.
- Auto-publishing customer-facing content.
- Becoming a code editor — Cortex shows diffs, has a small CodeMirror editor and a terminal, but agents edit files and the user reviews in their editor of choice.
