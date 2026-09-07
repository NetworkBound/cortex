# Privacy

Cortex is a single-user app. This doc records what data leaves the user's devices and under what conditions.

## What stays local (always)

- Chat content sent to Cortex's UI.
- Memory file contents and indexes.
- Audit log of agent actions.
- Cached span/telemetry data.
- API keys, SSH keys, project source code.

## What leaves the device

| Destination | When | Content |
|---|---|---|
| Cortex Gateway backend (`http://gateway.example:8642`) | Every chat turn routed there | Chat messages + selected history (per request). It's the user's own LAN service. |
| Anthropic / OpenAI / Gemini APIs | Only if an adapter is configured to call them directly (default: no — use the gateway) | Chat messages + tool calls |
| Ollama / ComfyUI / etc. on a local inference host | When the orchestrator routes media/text generation there | The relevant prompt / image |
| Sentry SaaS (`sentry.io`) | **Only if user opts in.** Off by default. | App crash stack traces + metadata. **Never** chat content, prompts, tool args, file paths' contents, or memory bodies. |
| Other devices on the user's Tailscale | When sync runs (default: every 5 min) | Memory directories, runbooks. No secrets, no chroma DB. |

## The Sentry `beforeSend` filter

Implemented in `src-tauri/src/observability/sentry.rs` (Phase 4). Strips:

- Any payload field whose key matches `/message|content|prompt|body|args|result/i`.
- Any string longer than 256 characters.
- Any string matching common API key shapes (`sk-`, `pk-`, `xoxb-`, `ghp_`, `Bearer ...`).

If you ever see chat content in a Sentry event, that's a bug — file it.

## Opt-in flows

- First-run: a single screen explains crash reporting and lets you enable or skip. Default is **skip**.
- Settings → Privacy: toggle anytime.
- Settings → Privacy → "Export and review what would be sent" runs the redactor on a sample crash and shows you exactly the payload.

## What we don't do

- No analytics, no metrics on app usage to any third party.
- No telemetry on which agents you use, how often, or for what.
- No call-home on update check (the updater hits GitHub Releases directly).

## If you change your mind

Sentry data is purgeable from the Sentry dashboard. Tailscale-synced memory is in your control (delete on any device, the others propagate the delete on next sync). The local Cortex DB lives at `~/.local/share/cortex/` — delete the directory and Cortex starts fresh.
