# Cortex Security Notes

This is the operator-facing summary of how Cortex handles secrets and trust.

## Threat model (lite)

Cortex is a high-trust app: it owns API keys, can spawn agents that run shell
commands, and indexes your local memory. The realistic threats:

| Threat | Mitigation |
|---|---|
| Compromised npm/crate dep exfiltrates secrets via the renderer | Renderer has zero secret access; CSP `connect-src` is restricted to localhost |
| Prompt injection from web/file content fed to an agent tricks it into reading SSH keys | Curated env, cwd pinned to project root, file-scope allowlist in the Tauri capability |
| Agent CLI is itself compromised | OS keychain holds secrets, audit log catches surprising writes, no blanket FS scope |
| Crashes leak chat content to Sentry | `beforeSend` strips message/content/prompt fields and token-shaped strings |
| `--dangerously-skip-permissions` left on permanently | UI toggle is session-bound, 30-min timeout, persistent banner |
| Auto-update pushes a malicious binary | Updater verifies an ed25519 signature against a pinned pubkey before swapping |
| One device sync overwrites memory on another | Per-write backups (last 5 versions) |

## What lives where

- Secrets → OS keychain (`keyring` crate). Never on disk.
- Memory contents → existing files in `~/.claude/projects/*/memory/`, etc. Cortex indexes but does not duplicate.
- Chat history → local SQLite under your data dir. Device-local.
- Audit log → append-only, under your data dir.
- Infrastructure endpoints → `~/.cortex/infra.json` (or env vars). No endpoints are baked into the binary.

## What you should rotate if your install is compromised

In order of priority:

1. Your gateway API key (`/v1/*` access), if you use one.
2. Any Anthropic, OpenAI, or Gemini API keys you configured.
3. Any SSH key a spawned agent could reach (Cortex can't read `~/.ssh` directly, but a spawned `claude`/`codex` *can*).

## Hardening still on the roadmap

- Bubblewrap / Firejail wrapper for agent subprocesses on Linux.
- Notarized + signed builds for all OSes.
- `cargo audit` and `pnpm audit` gates in CI.
- An "incognito" session mode that disables memory write-back.

## Reporting

If you find a security issue, please open a GitHub issue (or a private security
advisory for sensitive reports).
