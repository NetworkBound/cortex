# Cortex Architecture

## One-line goal

A single desktop window where you send a task, the **orchestrator** decides which agent (or agents) should handle it, those agents execute against the right project, and every step shows up in a Sentry-style observability panel below the chat.

## Big picture

```
┌───────────────────────────── Cortex Desktop App ─────────────────────────────┐
│                                                                              │
│  ┌──────────────┐   ┌───────────────────────────┐   ┌─────────────────┐      │
│  │ Project list │   │       Chat pane           │   │  Agent sidebar  │      │
│  │  (Phase 5)   │   │  (streams agent events)   │   │ (capability /   │      │
│  │              │   │                           │   │  health view)   │      │
│  └──────┬───────┘   └────────────┬──────────────┘   └────────┬────────┘      │
│         │                        │                            │              │
│         └────── Tauri IPC ───────┴────────────────────────────┘              │
│                                  │                                           │
│                        ┌─────────▼──────────┐                                │
│                        │  Rust backend      │                                │
│                        │  (this binary)     │                                │
│                        └─┬───────┬────────┬─┘                                │
│                          │       │        │                                  │
│              ┌───────────▼──┐ ┌──▼─────┐ ┌▼─────────────┐                    │
│              │ Orchestrator │ │ Memory │ │ Observability│                    │
│              │   + routing  │ │ layer  │ │  (OTel-ish)  │                    │
│              └──────┬───────┘ └────┬───┘ └──────┬───────┘                    │
│                     │              │            │                            │
│           ┌─────────┼──────────────┼────────────┼──────────────────┐         │
│           │         │              │            │                  │         │
│   ┌───────▼──┐  ┌───▼────┐  ┌──────▼─────┐ ┌────▼─────┐  ┌─────────▼──┐     │
│   │ Claude   │  │ Codex  │  │  Gateway   │ │ Ollama   │  │  Local      │     │
│   │ CLI      │  │ CLI    │  │  remote    │ │ (local/  │  │  SQLite     │     │
│   │ (sub-    │  │ (sub-  │  │  /v1/...   │ │  remote) │  │  store      │     │
│   │ process) │  │ process│  │ (your URL) │ │ /api/... │  │ (~/.cortex) │     │
│   └──────────┘  └────────┘  └────────────┘ └──────────┘  └─────────────┘     │
└──────────────────────────────────────────────────────────────────────────────┘
                  │                  │                │
                  ▼                  ▼                ▼
        ~/projects/* files    ~/.claude/* memory   your service targets
        (working dirs for     (chroma DB,          (health pollers you
        spawned agents)       markdown memory,     configure in infra.json)
                              runbooks)
```

## Module boundaries

### Frontend — `src/` (React + TS, Vite)

| Module | Responsibility |
|---|---|
| `App.tsx` | Top-level layout: project sidebar, chat pane + observability, agent sidebar |
| `components/ChatPane.tsx` | Conversation rendering + input. Subscribes to streaming `agent-event` Tauri events |
| `components/AgentSidebar.tsx` | Lists agents with health/capability badges |
| `components/ProjectSidebar.tsx` | Project discovery + active-project selection (Phase 5) |
| `components/ObservabilityPanel.tsx` | Span waterfall, token usage, error list (Phase 4) |
| `components/CommandPalette.tsx` | Cmd/Ctrl+K palette — Zed-style (Phase 5) |
| `lib/agents.ts` | Frontend mirror of agent descriptors, calls `list_agents` |
| `lib/gateway.ts` | Thin wrapper around `invoke('chat_send')` and event subscription |
| `lib/memory.ts` | Calls `list_memory_files`, fetches body on demand |

### Backend — `src-tauri/src/` (Rust)

| Module | Responsibility |
|---|---|
| `lib.rs` | Tauri app bootstrap, plugin registration, command handlers |
| `agents/adapter.rs` | `AgentAdapter` trait, `ChatRequest`, `AgentEvent` |
| `agents/registry.rs` | Holds `Arc<dyn AgentAdapter>` instances; lookup by id |
| `agents/claude.rs` | Spawns `claude --print --output-format stream-json` subprocess |
| `agents/codex.rs` | Spawns `codex` subprocess (Phase 2) |
| `agents/gateway_remote.rs` | OpenAI-compatible client against your configured gateway `…/v1/*` |
| `agents/ollama.rs` | Direct `/api/chat` POSTs to your configured Ollama server |
| `gateway/client.rs` | Reusable HTTP client (auth, retries, SSE parsing) |
| `commands/*.rs` | Tauri IPC commands (the only thing the renderer can call) |
| `memory/markdown.rs` | Parse frontmatter, follow `[[wikilinks]]`, watch for changes |
| `memory/chroma.rs` | Read claude-mem chroma DB; semantic search (Phase 3) |
| `memory/sync.rs` | Optional cross-device sync over Tailscale or Supabase (Phase 3) |
| `observability/tracing_store.rs` | SQLite-backed span store (Phase 4) |
| `observability/sentry.rs` | Sentry SDK init (opt-in) (Phase 4) |
| `observability/homelab.rs` | Health pollers for user-configured service targets (Phase 4) |

## Data flow — single message, single agent

1. User types in `ChatPane`, hits Cmd+Enter.
2. Frontend calls `invoke('chat_send', { session_id, message, agent: null, project_root })`.
3. Backend's `commands::chat::chat_send` enqueues into the orchestrator with `agent: null` (let the orchestrator pick).
4. Orchestrator inspects message + project context + agent capabilities, picks one (or more).
5. Orchestrator spawns an async task that calls `adapter.run(req, tx)` for each picked agent.
6. As `AgentEvent`s arrive, the backend `emit`s them on a per-session Tauri event channel.
7. `ChatPane` subscribes to `agent-event:{session_id}` and re-renders.
8. Each event is also written to the span store for the observability panel.

## Data flow — fan-out (two agents on same project)

1. Same as above through step 4.
2. Orchestrator picks ≥2 agents and labels them with roles ("primary", "reviewer", or task split).
3. Each runs in its own subprocess with the same `project_root` (or a copy-on-write worktree, Phase 2 stretch).
4. Events from each are interleaved in the chat under the agent name; the observability panel shows a parent span with one child per agent.
5. On completion, a synth step asks the gateway-remote agent to summarize/reconcile.

## IPC contract (renderer ↔ backend)

Renderer **only** invokes these commands:

| Command | Args | Returns |
|---|---|---|
| `chat_send` | `{session_id, message, agent?, project_root?}` | `ChatSendResult` (events stream separately) |
| `list_agents` | — | `AgentDescriptor[]` |
| `list_memory_files` | — | `MemoryFile[]` |
| `get_gateway_config` | — | `{base_url, has_api_key}` |
| `set_gateway_api_key` | `{api_key}` | `()` (stored in OS keychain) |
| `list_projects` | — (Phase 5) | `Project[]` |
| `set_active_project` | `{path}` (Phase 5) | `()` |

Events the backend emits:

| Event | Payload |
|---|---|
| `agent-event:{session_id}` | `AgentEvent` (see `adapter.rs`) |
| `span-update` | partial `Span` (Phase 4) |
| `health-update` | per-agent / per-host health (Phase 4) |

## Storage

| What | Where | Format |
|---|---|---|
| Chat history | `~/.local/share/gateway/cortex-local.db` | SQLite |
| Spans / telemetry | same DB, separate tables | SQLite |
| Settings | `~/.config/gateway/settings.json` (via tauri-plugin-store) | JSON |
| Secrets | OS keychain (Secret Service / Keychain / Credential Vault) | binary |
| Source-of-truth memory | `~/.claude/projects/*/memory/`, `~/.claude-mem/chroma`, `~/projects/*/runbooks/`, optional Obsidian vault | external, read mostly |

Cortex does **not** duplicate your existing memory — it indexes and watches. The local DB is for what's app-specific (chat history, spans).

## What's NOT in scope (deliberately)

- Hosting models locally — run your own Ollama/inference server.
- Replacing the gateway — when configured, this app is a client of it.
- Becoming a code editor — for editing, agents work on files and you review in your editor of choice. Cortex shows diffs but doesn't replace those editors.
