import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";

/**
 * Settings card for **Cortex as an MCP server** — the `POST /mcp` endpoint on
 * the embedded mobile server that lets Claude Code / Codex / Gemini CLI use
 * the Brain and checkpoints from a terminal.
 *
 * Off by default. Enabling mints a bearer token (kept in the encrypted key
 * vault); the raw token only crosses the bridge when the user explicitly asks
 * for the paste-ready snippets.
 */

interface McpServerStatus {
  enabled: boolean;
  allow_destructive: boolean;
  url: string;
  has_token: boolean;
  token_masked: string;
}

interface ClientSnippets {
  url: string;
  token: string;
  claude_code: string;
  codex_toml: string;
  gemini_json: string;
}

async function copyText(label: string, text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    pushToast({ title: `${label} copied`, kind: "success" });
  } catch (e) {
    pushToast({ title: `Copy failed: ${humanizeError(e)}`, kind: "error" });
  }
}

export default function McpServerSettings() {
  const [status, setStatus] = useState<McpServerStatus | null>(null);
  const [snippets, setSnippets] = useState<ClientSnippets | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await invoke<McpServerStatus>("mcp_server_get_config");
        if (!cancelled) setStatus(s);
      } catch (e) {
        if (!cancelled) setErr(humanizeError(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  async function apply(
    cmd: string,
    args?: Record<string, unknown>,
  ): Promise<void> {
    setBusy(true);
    setErr(null);
    try {
      const s = await invoke<McpServerStatus>(cmd, args);
      setStatus(s);
      // Any change to the token invalidates the snippets shown.
      setSnippets(null);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function showSnippets(): Promise<void> {
    setBusy(true);
    setErr(null);
    try {
      const s = await invoke<ClientSnippets>("mcp_server_client_snippets");
      setSnippets(s);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  const enabled = status?.enabled ?? false;

  return (
    <div className="settings-section">
      <h3>Cortex as MCP server</h3>
      <div className="settings-hint spaced">
        Expose the Brain (semantic search + grounded answers), project
        checkpoints, recent sessions and run reliability as MCP tools so Claude
        Code, Codex and Gemini CLI running in a terminal can use them. Served on
        the local mobile server (<code>{status?.url ?? "…/mcp"}</code>),
        loopback only, bearer-token protected. Nothing leaves this machine.
      </div>
      {err && <div className="settings-err">{err}</div>}
      <label className="settings-check">
        <input
          type="checkbox"
          checked={enabled}
          disabled={busy || status === null}
          onChange={(e) =>
            void apply("mcp_server_set_enabled", {
              enabled: e.target.checked,
              allowDestructive: null,
            })
          }
        />
        <span>Enable MCP server</span>
      </label>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={status?.allow_destructive ?? false}
          disabled={busy || status === null}
          onChange={(e) =>
            void apply("mcp_server_set_enabled", {
              enabled,
              allowDestructive: e.target.checked,
            })
          }
        />
        <span>
          Allow destructive tools
          <small>
            Exposes <code>checkpoint_restore</code>, which overwrites a
            project's working tree.
          </small>
        </span>
      </label>
      {status && (
        <div className="settings-row wrap spaced">
          <span className="settings-label-sm">Token</span>
          <code>{status.has_token ? status.token_masked : "(none yet)"}</code>
          <button
            type="button"
            className="btn-secondary"
            disabled={busy}
            onClick={() => void apply("mcp_server_rotate_token")}
          >
            Rotate
          </button>
          <button
            type="button"
            className="btn-secondary"
            disabled={busy}
            onClick={() => void showSnippets()}
          >
            {snippets ? "Refresh snippets" : "Show client snippets"}
          </button>
          {snippets && (
            <button
              type="button"
              className="btn-ghost"
              onClick={() => setSnippets(null)}
            >
              Hide
            </button>
          )}
        </div>
      )}
      {snippets && (
        <div className="gap-top">
          <div className="settings-hint">
            These contain the raw token — treat them like a password. Rotating
            the token invalidates every client until re-pasted.
          </div>
          {(
            [
              ["Claude Code", snippets.claude_code],
              ["Codex (~/.codex/config.toml)", snippets.codex_toml],
              ["Gemini CLI (~/.gemini/settings.json)", snippets.gemini_json],
            ] as const
          ).map(([label, text]) => (
            <div key={label} className="gap-top">
              <div className="settings-row wrap">
                <span className="settings-label-sm">{label}</span>
                <button
                  type="button"
                  className="btn-ghost"
                  onClick={() => void copyText(label, text)}
                >
                  Copy
                </button>
              </div>
              <pre
                className="settings-mono"
                style={{ maxHeight: 140, overflow: "auto" }}
              >
                {text}
              </pre>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
