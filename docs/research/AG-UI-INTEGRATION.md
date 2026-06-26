# AG-UI Protocol Integration Research

**Status:** Research / Pre-implementation
**Date:** 2026-05-25
**Target:** Cortex desktop app (Tauri + React + Rust), remote agent gateway
**Spec source:** https://github.com/ag-ui-protocol/ag-ui, https://docs.ag-ui.com

---

## 1. What AG-UI Is

AG-UI ("Agent–User Interaction Protocol") is, in the project's own words, *"an open, lightweight, event-based protocol that standardizes how AI agents connect to user-facing applications."* It sits alongside MCP (which gives agents *tools*) and A2A (agent-to-agent comms) in what the project calls the "Agent Protocol Stack" — AG-UI is the layer that brings agents into **user-facing apps**.

The problem it solves is the impedance mismatch between traditional request/response web APIs and the way agents actually behave: long-running, streaming, nondeterministic, mixed structured/unstructured I/O, and frequently human-in-the-loop. Rather than every frontend re-inventing event names for "the assistant streamed a token", "a tool started", "the agent wants the user to confirm something", AG-UI defines ~26 standard event types and a small set of message envelopes, transported over SSE (text) or a binary protocol over HTTP POST. Servers (agents) emit; clients (UIs) subscribe, render, and feed tool results / user input back in. Official SDKs exist for TypeScript, Python, Kotlin, Java, Go, Rust, Ruby, C++, and Dart; the Rust SDK lives at `sdks/community/rust/crates/{ag-ui-core, ag-ui-client}`.

## 2. Wire Format

### 2.1 Transport

- Single HTTP `POST` endpoint on the agent server. Request body is a JSON `RunAgentInput`. Response is a stream of events.
- Two transports advertised via the request's `Accept` header:
  - `text/event-stream` — SSE, JSON-encoded events in the `data:` field. Default.
  - A binary protocol (protobuf-based) for production. The TS SDK's `EventEncoder` picks the format from `Accept`.
- The TS client (`HttpAgent`) sets `Accept: text/event-stream` by default.

### 2.2 Run input (`RunAgentInput`)

The TS type is:

```ts
type RunAgentInput = {
  threadId: string
  runId: string
  parentRunId?: string
  state: any
  messages: Message[]
  tools: Tool[]
  context: Context[]
  forwardedProps: any
}
```

`Tool` is `{ name: string; description: string; parameters: any }` (JSON Schema). `Context` is `{ description: string; value: string }`. Tools are **defined by the frontend** and shipped to the agent on each run — this is the mechanism that powers human-in-the-loop (see §4).

### 2.3 Message envelope

Shared fields on every `Message`:

- `id: string` — unique message id
- `role: "user" | "assistant" | "system" | "tool" | "developer" | "reasoning" | ...`
- `content?: string`
- `name?: string`
- `encryptedContent?: string` (for privacy-preserving state continuity)

Variant types: `UserMessage`, `AssistantMessage` (may carry `toolCalls`), `SystemMessage`, `DeveloperMessage`, `ToolMessage` (must carry `toolCallId`, may carry `error`), `ActivityMessage`, `ReasoningMessage`.

`ToolCall` shape:

```ts
type ToolCall = {
  id: string
  type: "function"
  function: { name: string; arguments: string /* JSON */ }
  encryptedValue?: string
}
```

### 2.4 BaseEvent and event types

All events extend:

```ts
{
  type: EventType            // discriminator, SCREAMING_SNAKE_CASE constants
  timestamp?: number
  rawEvent?: any             // original event if transformed via middleware
}
```

The `EventType` enum, grouped:

| Category | Events | Required fields beyond `type` |
|----------|--------|-------------------------------|
| **Lifecycle** | `RUN_STARTED` | `threadId`, `runId` |
|  | `RUN_FINISHED` | (optional `outcome`) |
|  | `RUN_ERROR` | `message`; optional `code` |
|  | `STEP_STARTED` / `STEP_FINISHED` | `stepName` |
| **Text** | `TEXT_MESSAGE_START` | `messageId`, `role` |
|  | `TEXT_MESSAGE_CONTENT` | `messageId`, `delta` |
|  | `TEXT_MESSAGE_END` | `messageId` |
|  | `TEXT_MESSAGE_CHUNK` | (all optional — convenience form) |
| **Tool calls** | `TOOL_CALL_START` | `toolCallId`, `toolCallName` |
|  | `TOOL_CALL_ARGS` | `toolCallId`, `delta` (string fragments of a JSON object) |
|  | `TOOL_CALL_END` | `toolCallId` |
|  | `TOOL_CALL_RESULT` | `toolCallId`, `content` |
|  | `TOOL_CALL_CHUNK` | (convenience form) |
| **State** | `STATE_SNAPSHOT` | `snapshot` (full state) |
|  | `STATE_DELTA` | `delta` (RFC-6902 JSON Patch array of `{op, path, value, from}`) |
|  | `MESSAGES_SNAPSHOT` | `messages` |
| **Activity** | `ACTIVITY_SNAPSHOT` | `messageId`, `activityType`, `content` |
|  | `ACTIVITY_DELTA` | `messageId`, `activityType`, `patch` |
| **Reasoning** | `REASONING_START` / `REASONING_END` | `messageId` |
|  | `REASONING_MESSAGE_START` | `messageId`, `role` |
|  | `REASONING_MESSAGE_CONTENT` | `messageId`, `delta` |
|  | `REASONING_MESSAGE_END` / `REASONING_MESSAGE_CHUNK` | `messageId` |
|  | `REASONING_ENCRYPTED_VALUE` | `subtype`, `entityId`, `encryptedValue` |
| **Misc** | `RAW` | `event` (passthrough); optional `source` |
|  | `CUSTOM` | `name`, `value` (escape hatch) |
|  | `META_EVENT` (draft) | `metaType`, `payload` |

### 2.5 Streaming model

A run emits a tree of events, conventionally:

1. `RUN_STARTED { threadId, runId }`
2. Zero or more of: `STEP_STARTED`, text triplets (`TEXT_MESSAGE_START` → many `TEXT_MESSAGE_CONTENT` → `TEXT_MESSAGE_END`), tool-call triplets (`TOOL_CALL_START` → many `TOOL_CALL_ARGS` → `TOOL_CALL_END` → optionally `TOOL_CALL_RESULT`), state events, reasoning triplets.
3. `RUN_FINISHED` or `RUN_ERROR`.

`TEXT_MESSAGE_CHUNK` and `TOOL_CALL_CHUNK` exist as compact alternatives where the start/content/end can be inferred from monotonically appearing `messageId` / `toolCallId`.

## 3. Client and Server Roles

- **Client (frontend) initiates.** It constructs a `RunAgentInput` (with a chosen `threadId`/`runId`, history `messages`, the frontend-defined `tools`, and any `context`), POSTs it to the agent, and subscribes to the SSE stream.
- **Server (agent) responds with events**, never with a final JSON blob. It is purely a streaming emitter.
- **Tool execution lives on the client** for client-defined tools. The agent emits `TOOL_CALL_*`; the client runs the tool locally; the client appends a `ToolMessage { role: "tool", toolCallId, content }` to the history and POSTs a **new run** continuing from where the previous one stopped. This is the same pattern the OpenAI/Anthropic tool-use APIs use.
- The server *can* also execute server-side tools and emit `TOOL_CALL_RESULT` itself; the protocol doesn't forbid either model.
- The TS SDK exposes this through `AbstractAgent` (extend + implement `run()`) and `HttpAgent` (POSTs to a URL, parses SSE into typed events, exposes them as an RxJS Observable plus an `AgentSubscriber` listener model).

## 4. Tool Calls and Approvals — vs Cortex/Gateway Today

### 4.1 AG-UI

Standard lifecycle for *any* tool:

```
TOOL_CALL_START (toolCallId, toolCallName)
TOOL_CALL_ARGS  (toolCallId, delta)   [streamed, accumulate into JSON]
TOOL_CALL_END   (toolCallId)
→ frontend executes the tool →
ToolMessage { role: "tool", toolCallId, content }  (sent in the next run)
TOOL_CALL_RESULT (toolCallId, content)   [optional, server-emitted]
```

**There are no dedicated approval events.** The pattern is: define a tool such as `confirmAction(reason, preview, choices)` in the frontend's `tools` array. When the agent wants approval, it calls that tool; the frontend renders an approval card, captures the user's choice, and returns it as the tool result. From the protocol's perspective an approval is just another client-side tool call.

### 4.2 Cortex/Gateway today

Cortex talks to the gateway via `/v1/runs` with a custom SSE shape decoded into `RunStreamItem` (`gateway/client.rs`) and then bridged to `AgentEvent` (`agents/adapter.rs`). The gateway emits explicit, separate events for tools **and** approvals:

```rust
// agents/adapter.rs
pub enum AgentEvent {
    Started { agent_id, run_id },
    Token { delta },
    Reasoning { text },
    ToolCall { name, args, preview },
    ToolResult { name, ok, summary, duration_ms },
    FileEdit { path, lines_changed },
    ApprovalRequest { run_id, tool, preview, choices, request },
    ApprovalResolved { run_id, choice },
    Error { message },
    Done { total_tokens, run_id },
}
```

So Cortex/Gateway is **more opinionated than AG-UI**: it has first-class `ApprovalRequest`/`ApprovalResolved` events (closer to MCP's elicitation idea) rather than re-using the tool-call channel. AG-UI's design choice is the opposite — collapse approvals into tool calls so middleware doesn't have to know about them.

## 5. Mapping AG-UI → Cortex `AgentEvent`

| AG-UI event | Cortex equivalent | Gap |
|-------------|-------------------|-----|
| `RUN_STARTED { threadId, runId }` | `AgentEvent::Started { agent_id, run_id }` | Add `thread_id` field; Cortex uses `session_id` in `ChatRequest`. Maps cleanly. |
| `RUN_FINISHED` | `AgentEvent::Done { total_tokens, run_id }` | Optional `outcome` field not modelled — fine, ignore. |
| `RUN_ERROR { message, code? }` | `AgentEvent::Error { message }` | Add optional `code`. |
| `STEP_STARTED` / `STEP_FINISHED` | *none* | New variants needed if we want to render step boundaries (multi-agent / sub-agent). Low priority. |
| `TEXT_MESSAGE_START` | implicit (Cortex uses `Token` deltas only) | Need to emit a start marker when role changes / a new assistant message begins. |
| `TEXT_MESSAGE_CONTENT { delta }` | `AgentEvent::Token { delta }` | Exact match. |
| `TEXT_MESSAGE_END` | implicit | Need explicit end marker tied to a `messageId`. |
| `TOOL_CALL_START { toolCallId, toolCallName }` | first half of `AgentEvent::ToolCall` | Cortex doesn't carry a stable `toolCallId` — add it. |
| `TOOL_CALL_ARGS { toolCallId, delta }` | *none* (Cortex sends args as a single complete `serde_json::Value`) | Either accumulate-then-emit on Cortex side, or split into deltas — see §6. |
| `TOOL_CALL_END { toolCallId }` | implicit (Cortex's `ToolCall` is one-shot) | Add boundary if we want to be an AG-UI server. |
| `TOOL_CALL_RESULT { toolCallId, content }` | `AgentEvent::ToolResult { name, ok, summary, duration_ms }` | Need `tool_call_id` correlation, not just name. |
| `STATE_SNAPSHOT` / `STATE_DELTA` | *none* | Cortex has no synced agent state model. Optional — only needed for "generative UI" features. |
| `MESSAGES_SNAPSHOT { messages }` | *none* | Could be derived from chat store. |
| `ACTIVITY_SNAPSHOT` / `ACTIVITY_DELTA` | *none* | Skip unless we want long-running activity UIs. |
| `REASONING_MESSAGE_CONTENT { delta }` | `AgentEvent::Reasoning { text }` | Cortex passes full text per chunk; AG-UI streams deltas. Trivial. |
| `REASONING_START` / `REASONING_END` | implicit | Add markers. |
| `REASONING_ENCRYPTED_VALUE` | *none* | Optional. |
| *(no event)* — approval is a tool call | `AgentEvent::ApprovalRequest { run_id, tool, preview, choices, request }` | **Asymmetric.** To speak AG-UI outbound, Cortex must repackage `ApprovalRequest` as `TOOL_CALL_*` for a synthetic `confirmAction`-style tool. |
| `ApprovalResolved` | `AgentEvent::ApprovalResolved` | Inbound: ship the user's choice back as a `ToolMessage`. |
| `RAW` / `CUSTOM` | *none* | Useful escape hatches; map `FileEdit` to `CUSTOM { name: "file_edit", value: { path, lines } }` initially. |

**What Cortex would need to add to the `AgentEvent` enum** (or to a parallel AG-UI-shaped event type the adapter emits):

1. A stable `message_id` and `tool_call_id` carried through the stream.
2. `TextMessageStart`/`TextMessageEnd` boundaries (or just synthesize them in the encoder).
3. `StepStarted`/`StepFinished` if/when the gateway ever exposes sub-runs.
4. Optional: `StateSnapshot`/`StateDelta` for shared mutable state with the frontend.

## 6. Integration Plan

### 6.1 Recommendation: do both (a) and (b), but stagger them. Start with (b) — Cortex as AG-UI server.

**Why (b) first:**
- Cortex already *has* the streaming event source via the gateway. The work is purely a translation/encoding layer.
- Owner's stated goal is "compatible with the broader agent-UI ecosystem". The ecosystem ships **frontends** (CopilotKit, ag-ui-dojo, custom React apps); making Cortex expose AG-UI lets any of them talk to a Cortex session. This is the higher-leverage direction.
- It cleanly separates "the gateway-specific event shape" from "the public AG-UI event shape" — Cortex becomes a typed middleware.

**Why (a) (Cortex as AG-UI client) is a fast follow:**
- It would let Cortex point at *any* AG-UI-compliant agent (LangGraph servers, CrewAI, Claude Agent SDK middleware, Pydantic AI, etc.) alongside the gateway.
- The Rust crate `ag-ui-client` already exists (`sdks/community/rust/crates/ag-ui-client`), so this is a "wire it up" job, not a "design a protocol" job.
- Adds value as soon as someone wants to test Cortex against a non-gateway backend.

**Why NOT just (a):** doing only client-mode locks Cortex out of the ecosystem's frontends and misses the strategic interop story.

### 6.2 Phase B — Cortex exposes AG-UI (server mode)

Concrete file-level changes:

1. **New module:** `src-tauri/src/agui/` with:
   - `mod.rs` — public types matching AG-UI: `EventType`, `BaseEvent`, the specific event structs, `RunAgentInput`, `Message`, `ToolCall`, `Tool`, `Context`. Use `#[serde(tag = "type")]` with SCREAMING_SNAKE_CASE renaming to match the wire format.
   - `encoder.rs` — `EventEncoder` that takes `&AGUIEvent` and writes either SSE (`event:`/`data:` framing with JSON body) or the protobuf binary form. Mirror `Accept` header negotiation.
   - `from_cortex.rs` — translation from `AgentEvent` → one-or-more AG-UI events. This is where we synthesize `TextMessageStart`/`End`, mint stable `messageId`/`toolCallId` (use ULIDs), and re-encode `ApprovalRequest` as a `TOOL_CALL_*` triplet for a synthetic tool named e.g. `cortex.approval`.
2. **New HTTP server:** `src-tauri/src/agui/server.rs` exposing a single `POST /agui/run` endpoint. Use `axum` (already in the workspace via Tauri's tower stack — verify) bound to `127.0.0.1` on a configurable port, behind a token. The handler:
   - parses `RunAgentInput`,
   - looks up or creates a Cortex `ChatRequest` from the `messages` array (last user message → `req.message`, prior turns → `req.history`),
   - calls the registered `GatewayRemoteAgent` adapter,
   - pipes the resulting `mpsc::Receiver<AgentEvent>` through `from_cortex` and `encoder` into the SSE response.
3. **Tool-result re-entry:** when the AG-UI client sends a follow-up run whose last message has `role: "tool"`, route that back into the gateway as the approval response (when `toolCallId` matches an open approval) via the existing approval-resolution path in `commands/agents.rs`. Otherwise treat it as plain history.
4. **Settings UI:** add an "Expose Cortex via AG-UI" toggle + port field to `SettingsModal.tsx`. Default off.
5. **Docs:** `docs/AGUI-SERVER.md` (when implementation is done — *not* in this research phase).

### 6.3 Phase A — Cortex as AG-UI client (later)

1. **New adapter:** `src-tauri/src/agents/agui_remote.rs` implementing `AgentAdapter` against the `ag-ui-client` crate (or hand-rolled `reqwest` SSE — `ag-ui-client` is v0.1.0, evaluate stability first).
2. **Translation `to_cortex.rs`** inverse of `from_cortex.rs`: AG-UI events → `AgentEvent`. Synthetic-tool-as-approval recognized by tool-name convention (configurable, default `confirmAction|approvalRequest|cortex.approval`).
3. **Registry plumbing:** `src-tauri/src/agents/registry.rs` learns to instantiate `AGUIRemoteAgent` from a settings entry `{ kind: "agui", base_url, auth }`.
4. **UI:** agent picker in `AgentSidebar.tsx` already supports multiple adapters — just need a "Add AG-UI agent" form in settings.

### 6.4 Things explicitly out of scope for the first cut

- `STATE_SNAPSHOT`/`STATE_DELTA` (generative UI). Cortex has no shared-state surface today; punt until we do.
- Binary transport — ship SSE only; honor `Accept` but fall back to SSE if the client asks for protobuf.
- `ACTIVITY_*` events.
- `REASONING_ENCRYPTED_VALUE` — the gateway doesn't emit encrypted reasoning.

## 7. Open Questions / Unknowns

1. **Tool-args streaming.** The gateway currently emits a single tool-args blob via `ToolStarted`. AG-UI wants `TOOL_CALL_ARGS` deltas. Acceptable to send one `TOOL_CALL_ARGS` with the full JSON string and immediately `TOOL_CALL_END`? Probably yes — chunk semantics say accumulate, single chunk is degenerate-valid — but worth testing against `ag-ui-dojo` to confirm no frontend balks.
2. **Approval round-trip semantics.** AG-UI assumes the approval flow uses a fresh run that includes a `ToolMessage`. The gateway's existing approval flow is **in-place** on the same run-id. We must keep the gateway run paused, mint a fake `toolCallId`, and resume the gateway when the next AG-UI run arrives carrying the `ToolMessage` for that id. State management for "open approvals" needs a small in-memory map keyed by `toolCallId → (gateway_run_id, choices)`.
3. **Thread vs session.** The gateway uses `session_id` + `session_key` (per-project). AG-UI uses `threadId`. Likely 1:1, but we should make sure two AG-UI threads in the same Cortex session don't clobber the gateway's `previous_response_id` continuity.
4. **Authentication.** AG-UI has no defined auth scheme. We'll likely require `Authorization: Bearer <token>` set by Cortex on startup (similar to how Cortex talks to the gateway). Worth checking what `ag-ui-dojo` / CopilotKit assume.
5. **Rust SDK maturity.** `ag-ui-client` is v0.1.0 with a `TODO` file at the crate root. Need to read the actual source before depending on it; if it's thin we may prefer hand-rolling against `reqwest` + `eventsource-stream`. There is also no `ag-ui-server` crate — server-side encoding is on us.
6. **Schema versioning.** AG-UI doesn't appear to send a protocol-version field. Mismatches will look like "unknown event type" errors. Worth proposing a `protocolVersion` field upstream, or at minimum tracking the spec commit hash in our `agui/mod.rs` header.
7. **`FileEdit` mapping.** Cortex emits a domain-specific `FileEdit` event. Cleanest AG-UI mapping is `CUSTOM { name: "cortex.file_edit", value: {...} }`. Worth checking if there's a community-blessed name to use instead — there isn't one in the current spec, so `CUSTOM` is correct.
8. **Multi-modal content.** AG-UI mentions "multimodal input (images, audio, video, documents)" in `UserMessage`. Cortex `ChatTurn.content` is a plain `String`. Punt until we wire vision into the gateway.
9. **Encrypted state continuity.** `encryptedContent` on messages and `REASONING_ENCRYPTED_VALUE` exist for privacy-preserving state. Probably irrelevant for Cortex's local-first model, but flag it before we accidentally drop those fields.
10. **Outcome of `RUN_FINISHED`.** Spec says `outcome` is optional with no enumerated values — verify against the TS source what callers expect (`"success" | "error" | "cancelled"`?).

---

**Bottom line:** Cortex should expose AG-UI as a server first (Phase B), translating gateway events through a new `src-tauri/src/agui/` module, because it has the highest ecosystem-interop payoff and doesn't require touching the gateway itself. Cortex-as-AG-UI-client (Phase A) follows as a second adapter alongside `GatewayRemoteAgent`. The biggest design decision is collapsing Cortex's first-class approval events into AG-UI's tool-call-based approval convention via a synthetic `cortex.approval` tool — that mapping needs to be the first thing we prototype.
