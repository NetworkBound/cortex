# Changelog

All notable changes to Cortex are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [3.2.0] — 2026-09-27

A hardening release: Windows and Linux are now both built and tested on every
push, and a full audit fixed the bugs that turned up.

### Fixed — Windows
- Agent CLIs installed through npm (`claude.cmd`, `codex.cmd`, …) are spawned
  without hand-built `cmd /C` command lines, so prompt text containing `&`,
  `|` or `"` can no longer break out into cmd.exe. No console window flashes
  per run, and stopping a run kills the CLI's whole process tree.
- `npm`, `npx`, `pnpm`, `pytest`, MCP servers and hooks resolve their `.cmd`
  shims, so tests, monitors, dependency audit, TypeScript diagnostics
  (`@problems`) and PRP validation gates work on Windows instead of reporting
  "not installed".
- Canonical `\\?\C:\` paths are normalised, so trusted projects, the last
  opened project and chat sharing into a project folder are recognised.
- `CORTEX_PROJECTS_ROOT` uses the OS path-list separator; `:` split drive
  letters apart.
- The terminal defaults to PowerShell 7, then Windows PowerShell, then cmd
  (`CORTEX_SHELL` overrides; on Linux/macOS it uses `$SHELL`).
- Browser session detection checks every Chromium profile and gives a clear
  message for App-Bound (v20) cookies.
- git pull/push/clone and Gitea backup can no longer hang on a hidden
  credential prompt.

### Fixed — everywhere
- A keyring hiccup (locked Secret Service, Credential Manager error) could
  overwrite the vault master key and make every stored provider key
  unreadable. Only a missing key now creates a new one, and vault writes are
  serialised.
- `@web` and `@websearch` crashed the send with a nested-runtime panic.
- The mobile web app is bundled into installers; installed builds used to
  serve a 404 for the phone UI.
- The mobile server's WebSocket checks `Origin`, so another website open in
  the same browser can't read streamed chat tokens.
- Gitea backup no longer mirrors the file holding its own access token.
- Clicking a note in the vault manager called a command that didn't exist.
- Event listeners leaked when a panel unmounted before `listen()` resolved
  (chat, settings, batch runner, routines, safe mode, terminal).
- The editor's file reads were always denied by the Tauri capability set.
- The SSH usage poller sent a literal `<gateway-ct>` placeholder; the
  container id is now configurable in `infra.json`.
- SQLite run history uses WAL and a busy timeout, so the desktop app and
  `cortex-serve` can share it.
- Test runs have a 20-minute timeout instead of hanging forever.

### Build and CI
- CI runs typecheck, ESLint, the frontend and mobile builds, `cargo check
  --all-targets` and `cargo test` on both Ubuntu and Windows.
- A fresh clone could not compile until the Go sidecar was built by hand;
  CI and the docs now cover it.
- Windows release builds failed on a maintainer-local signing certificate
  thumbprint. The release workflow imports a certificate from secrets when
  present and otherwise builds unsigned. Linux releases build on Ubuntu
  22.04 for a lower glibc floor.

### Removed
- The unused status bar and its six child components, the setting that
  toggled it, and about 430 lines of CSS no longer referenced anywhere.

## [3.1.0] — 2026-07-03

A release about run observability: seeing what agents actually did, how
reliable each model has been, and reaching any model you host. (Versioning
jumped from 0.2.x to match the app's marketing version.)

### Added
- Agent Reliability Dashboard (under Observability): per-provider and
  per-model success rate, p50/p95 latency, token totals, and estimated cost,
  aggregated from local run history. Time-range filter, CSV/JSON export, and
  failing rows link to the run that failed. Metrics are a local view — no
  visibility into gateway-internal retries — and cost is an estimate; the UI
  says so.
- Run Replay ("agent black box"): play back any past run as a timeline — the
  prompt, the routing decision, each tool call and approval, file edits,
  errors, result, and per-run cost. Export as redacted JSONL. Read-only.
- Homelab Model Fabric: register any OpenAI-compatible endpoint (vLLM,
  llama.cpp, LM Studio on LAN/tailnet, or a hosted API) from Settings →
  Providers → Model fabric. Health check, model discovery, latency test, and
  chat via a `fabric-<name>` agent. Reachability probes never send the API
  key. Routing is explicit for now: pick the endpoint's model in the composer.

### Fixed
- Gateway runs now record real token usage (previously zero), so cost and
  usage rollups reflect the primary path.
- The routing reason for each turn is captured and shown in Run Replay.

## [0.2.9] — 2026-06-28

### Fixed
- Layout mangle: chat pane collapsed to 0px when opening a project or chat
  (scrollIntoView + 1fr race). Replaced with scroll-margin + deferred scroll.
- Auto-update: manifest URL, Gitea asset detection, and signature validation
  all wired correctly for self-hosted Gitea releases.
- Duplicate user messages appearing in chat on send.
- ~30 additional HIGH/MEDIUM/LOW bugs from a full-app audit (scrollbar
  bleed, provider discovery, theme consistency, a11y, tooltip z-index, etc.).

### Added
- Image/file upload in chat composer: drag-drop, clipboard paste, and
  attach button. Images sent as base64 data URIs; text files as fenced
  code blocks.
- Voice-to-chat (mic button): browser SpeechRecognition primary, whisper-cli
  fallback. Works in Edge/Chromium webview out of the box.
- Thinking/working indicator shows the selected model name for all providers
  (e.g. "GPT 5.5 is thinking", "CLAUDE SONNET 4 is thinking").
- Project persistence: last active project restored on app restart.
- Native Windows project discovery: repos directly under ~ are found
  alongside ~/projects/*; WSL UNC paths de-duped in favor of native paths.

## [0.2.8] — 2026-06-25

### Added
- Ultimate multi-model agent: all 7 CLI agents + 13 OpenAI-compatible APIs +
  6 local runtimes routable from one chat. Model picker with slug search.
- Mobile web access: companion HTTP server for phone/tablet browsers.
- Cost-aware auto-routing: capability-gated first, then cheapest-capable wins.
- Gateway made fully optional: local CLI agents work standalone without a
  gateway deployment. `infra.json` configures only Ollama base URL.

## [0.2.5] — 2026-06-14

### Added
- Brain / Knowledge system: semantic search over chat history and Obsidian
  vault via local embeddings + vector store. `@brain` context token.
- Unified RAG: "chat with your brain" — ask questions across all stored
  knowledge, not just the current session.
- Save-to-Brain: save any message or selection to the knowledge base.

## [0.2.0] — 2026-06-03

### Added
- Teams: manager + specialist worker agents with automatic task decomposition.
- Lanes: same task across providers in isolated git worktrees.
- Arena: head-to-head A/B model comparison with persistent ELO leaderboard.
- MCP catalog: JSON-RPC MCP client + one-click server installation.
- Checkpoints + `/undo`: git-independent workspace snapshots with diff preview.
- Plan/Act mode toggle (Ctrl+M): plan mode blocks write/exec tools.
- `@` context tokens: `@brain`, `@diff`, `@recent`, `@status`, `@grep`, `@web`,
  `@summary`, `@file`, and more.
- Session auto-condense: long conversations automatically summarized to stay
  within context limits.
- Custom agent roles/profiles and mode customization.
- Architect mode (`/architect`): two-phase planner + editor split.

## [0.1.0] — 2026-05-20

### Added
- Initial release: Tauri 2 desktop app (Rust + React).
- Streaming chat with Claude CLI, Codex CLI, Gemini CLI, Ollama, and
  OpenAI-compatible gateway.
- Orchestrator with `@mention` routing, capability scoring, fan-out.
- Memory layer: project runbooks, CLAUDE.md, Obsidian vault integration.
- SQLite observability store (OpenTelemetry-shaped spans).
- Project discovery under ~/projects/*, file tree, command palette (Ctrl+K).
- Search across memory and chat history (Ctrl+Shift+F).
- Ed25519-signed auto-updates from Gitea.
- Audit log (JSONL, 90-day retention).
