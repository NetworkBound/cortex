import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { useCortexStore } from "@/state/store";
import {
  importChatFile,
  importChatPull,
  listRules,
  historySyncStatus,
  historySyncSetEnabled,
  historySyncNow,
  historySyncConnect,
  HISTORY_SYNC_PROGRESS_EVENT,
  type HistorySyncProgress,
  type ImportProvider,
  type ImportResult,
  type ProviderSyncStatus,
  type RuleSummary,
} from "@/lib/cortex-bridge";
import {
  listMonitors,
  startMonitors,
  stopMonitors,
  type MonitorSpec,
} from "@/lib/monitors";
import { ActivationBadge, SettingsSection, SettingsToggle } from "./Section";
import type { GatewayForm, SectionDef } from "./types";

function ObsidianSection({ gw }: { gw: GatewayForm }) {
  return (
    <SettingsSection title="Obsidian vault">
      <label>
        Obsidian vault path
        <input
          value={gw.obsidian}
          onChange={(e) => gw.setObsidian(e.target.value)}
          placeholder="auto-detected: ~/Documents/Cortex Brain"
        />
      </label>
      <div className="settings-hint">
        Cortex auto-detects <code>~/Documents/Cortex Brain</code> on first run.
        Per-project config lives under <code>.cortex/*</code> inside each
        workspace directory.
      </div>
    </SettingsSection>
  );
}

// ── Chat-history import ──────────────────────────────────────────────────────
// Self-contained so its local state doesn't bloat the modal render. File
// import goes through the native dialog (the repo already uses
// @tauri-apps/plugin-dialog); the experimental pull mirrors the mobile UX.

type ImportStatus =
  | { kind: "idle" }
  | { kind: "busy"; what: string }
  | { kind: "ok"; result: ImportResult }
  | { kind: "err"; message: string };

function ImportSettings() {
  const [status, setStatus] = useState<ImportStatus>({ kind: "idle" });
  const [provider, setProvider] = useState<ImportProvider>("claude");
  const [token, setToken] = useState("");

  const busy = status.kind === "busy";

  const run = async (what: string, fn: () => Promise<ImportResult>) => {
    setStatus({ kind: "busy", what });
    try {
      setStatus({ kind: "ok", result: await fn() });
    } catch (e) {
      setStatus({ kind: "err", message: humanizeError(e) });
    }
  };

  const pickFile = async () => {
    if (busy) return;
    const selected = await openDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Chat export", extensions: ["json"] }],
    });
    if (typeof selected !== "string") return; // cancelled
    void run("Importing file…", () => importChatFile(selected));
  };

  const pull = () => {
    const t = token.trim();
    if (!t || busy) return;
    void run(`Pulling from ${provider}…`, () => importChatPull(provider, t));
  };

  return (
    <SettingsSection
      title="Import chat history"
      description="Bring your Claude.ai or ChatGPT history into Cortex. Imported chats become resumable, searchable sessions in the sidebar."
    >
      {status.kind === "busy" && (
        <div className="settings-hint">{status.what}</div>
      )}
      {status.kind === "ok" && (
        <div className="settings-hint">
          ✓ Imported {status.result.imported}{" "}
          {status.result.imported === 1 ? "conversation" : "conversations"}
          {status.result.skipped > 0 && `, skipped ${status.result.skipped}`}.
        </div>
      )}
      {status.kind === "err" && (
        <div className="settings-err">{status.message}</div>
      )}

      <div className="settings-row spaced">
        <button type="button" onClick={() => void pickFile()} disabled={busy}>
          Choose export file…
        </button>
        <span className="settings-hint">
          A chat-export <code>.json</code> from Claude.ai, ChatGPT, or generic.
        </span>
      </div>

      <div className="settings-hint spaced">
        <strong>⚠️ Experimental / unofficial.</strong> Pull directly from your
        account with a session token. Uses fragile, unofficial endpoints that
        may break. The token is sent once to import and never stored or logged.
      </div>

      <label>
        Provider
        <select
          value={provider}
          onChange={(e) => setProvider(e.target.value as ImportProvider)}
          disabled={busy}
        >
          <option value="claude">Claude.ai</option>
          <option value="chatgpt">ChatGPT</option>
        </select>
      </label>

      <label>
        Session token
        <input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder={
            provider === "claude" ? "sessionKey value" : "accessToken value"
          }
          disabled={busy}
        />
      </label>
      <div className="settings-hint">
        {provider === "claude" ? (
          <>
            Claude.ai: copy the <code>sessionKey</code> cookie value (DevTools →
            Application → Cookies → claude.ai).
          </>
        ) : (
          <>
            ChatGPT: open <code>chatgpt.com/api/auth/session</code> while signed
            in and copy the <code>accessToken</code> value.
          </>
        )}
      </div>

      <div className="settings-row spaced">
        <button type="button" onClick={pull} disabled={busy || !token.trim()}>
          Pull from {provider === "claude" ? "Claude.ai" : "ChatGPT"}
        </button>
      </div>
    </SettingsSection>
  );
}

// Short relative "x ago" for the History sync rows, or "never" when absent.
function relTime(ms: number | null): string {
  return timeAgo(ms, { empty: "never", coarse: true });
}

/**
 * "History sync" — per-provider auto-sync of web chat history into Cortex.
 *
 * Unlike the manual {@link ImportSettings} pull (which needs a pasted token),
 * this captures the provider's *web* session automatically: browser cookie
 * auto-detect first, with a one-time in-app login fallback (the "Connect"
 * button → `historySyncConnect` opens a sign-in webview). The background loop
 * keeps history fresh on a schedule ONLY while a browser session stays
 * detectable; a webview Connect sign-in is a one-shot sync (its session is not
 * visible to the scheduler), so the user re-Connects when it expires. We poll
 * status every few seconds while any action is in flight so counts/last-synced
 * refresh live.
 */
function HistorySyncSection() {
  const [rows, setRows] = useState<ProviderSyncStatus[]>([]);
  const [busy, setBusy] = useState<string | null>(null); // provider key in flight
  const [msg, setMsg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // Live "fetched N of M conversations" progress from the webview-fetch login
  // fallback (the `history_sync:progress` event), keyed by provider.
  const [progress, setProgress] = useState<Record<string, string>>({});

  const refresh = async () => {
    try {
      setRows(await historySyncStatus());
    } catch (e) {
      setErr(humanizeError(e));
    }
  };

  // Initial load + light polling so background syncs surface without a reopen.
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      void (async () => {
        try {
          const s = await historySyncStatus();
          if (!cancelled) setRows(s);
        } catch {
          /* transient — leave the previous rows */
        }
      })();
    };
    load();
    const id = setInterval(load, 5000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  // Subscribe to live webview-fetch progress so the user sees "fetched N of M"
  // while the sign-in window is downloading their history.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      const off = await listen<HistorySyncProgress>(
        HISTORY_SYNC_PROGRESS_EVENT,
        (e) => {
          const p = e.payload;
          setProgress((prev) => ({ ...prev, [p.provider]: p.message }));
          if (p.phase === "done" || p.phase === "error") {
            // Let the final message linger briefly, then clear + refresh.
            setTimeout(() => {
              setProgress((prev) => {
                const next = { ...prev };
                delete next[p.provider];
                return next;
              });
              void refresh();
            }, 2500);
          }
        },
      );
      // The modal may have closed while the subscribe was in flight — tear the
      // listener down right away instead of leaking it for the app lifetime.
      if (disposed) off();
      else unlisten = off;
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  async function act(provider: string, fn: () => Promise<string | void>) {
    setErr(null);
    setMsg(null);
    setBusy(provider);
    try {
      const out = await fn();
      if (typeof out === "string") setMsg(out);
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <SettingsSection
      title="History sync"
      description={
        <>
          Pull your web chat history (Claude.ai, ChatGPT) into Cortex — no token
          to paste. When a browser session is detectable, Cortex keeps it
          updated on a schedule automatically. Otherwise use{" "}
          <strong>Connect</strong> to sign in and sync; you&apos;ll re-Connect
          when that sign-in expires. Sessions are never logged.
        </>
      }
    >
      {msg && <div className="settings-hint">{msg}</div>}
      {err && <div className="settings-err">{err}</div>}

      {rows.map((r) => {
        const inFlight = busy === r.provider;
        return (
          <div key={r.provider} className="settings-stack tight spaced">
            <SettingsToggle
              checked={r.enabled}
              disabled={inFlight}
              onChange={(next) =>
                void act(r.provider, () =>
                  historySyncSetEnabled(r.provider, next),
                )
              }
              label={`Sync history — ${r.label}`}
              description={
                <>
                  {r.conversation_count}{" "}
                  {r.conversation_count === 1
                    ? "conversation"
                    : "conversations"}{" "}
                  · last synced {relTime(r.last_sync)}
                  {r.session_source ? ` · via ${r.session_source}` : ""}
                </>
              }
            />
            {progress[r.provider] && (
              <div className="settings-hint">{progress[r.provider]}</div>
            )}
            <div className="settings-row wrap">
              <button
                type="button"
                onClick={() =>
                  void act(r.provider, () => historySyncNow(r.provider))
                }
                disabled={inFlight}
              >
                Sync now
              </button>
              {r.needs_login && (
                <button
                  type="button"
                  onClick={() =>
                    void act(r.provider, () => historySyncConnect(r.provider))
                  }
                  disabled={inFlight}
                >
                  Connect / Sign in
                </button>
              )}
            </div>
          </div>
        );
      })}
    </SettingsSection>
  );
}

/** `.cortex/rules/*.md` summaries for the active project. */
function ProjectRulesSection() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const root = activeProject?.root;
  const [rules, setRules] = useState<RuleSummary[]>([]);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!root) {
      setRules([]);
      setErr(null);
      return;
    }
    let cancelled = false;
    listRules(root)
      .then((list) => {
        if (!cancelled) {
          setRules(list);
          setErr(null);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setRules([]);
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  return (
    <SettingsSection
      title="Project rules"
      description={
        <>
          Markdown files in <code>.cortex/rules/</code> are pulled into every
          new chat for this project. Add a YAML frontmatter{" "}
          <code>activation</code> field to scope when each rule fires.
        </>
      }
    >
      {!activeProject && (
        <div className="settings-hint">
          Pick a project from the sidebar to see its rules.
        </div>
      )}
      {activeProject && err && <div className="settings-err">{err}</div>}
      {activeProject && !err && rules.length === 0 && (
        <div className="settings-hint">
          No rules found in <code>{activeProject.root}/.cortex/rules/</code>.
          Drop a <code>&lt;name&gt;.md</code> there to add one.
        </div>
      )}
      {activeProject && rules.length > 0 && (
        <ul className="settings-list">
          {rules.map((r) => (
            <li key={r.name} className="settings-list-row">
              <code>{r.name}</code>
              <ActivationBadge activation={r.activation} />
              {r.activation === "globs" && r.globs.length > 0 && (
                <small className="settings-muted settings-mono">
                  {r.globs.join(", ")}
                </small>
              )}
              {r.activation === "description" && r.description && (
                <small className="settings-muted">{r.description}</small>
              )}
            </li>
          ))}
        </ul>
      )}
    </SettingsSection>
  );
}

/**
 * Brain auto-context toggle. Subscribes to the store via the hook so the
 * checkbox re-renders when the value flips.
 */
function BrainSettingsSection() {
  const enabled = useCortexStore((s) => s.brainAutoEnabled);
  const setEnabled = useCortexStore((s) => s.setBrainAutoEnabled);
  return (
    <SettingsSection
      title="Brain"
      description={
        <>
          The local brain greps memory + recent edits + project files when you
          pause typing. Suggested @-tokens appear above the composer so you can
          click to attach. Disable below if you prefer to trigger brain context
          manually via the 🧠 button, slash commands, or <code>Alt+B</code>.{" "}
          <strong>Implicit path mentions</strong> (typing{" "}
          <code>src/auth.rs</code> directly into the draft) auto-attach up to 3
          files regardless of this setting.
        </>
      }
    >
      <SettingsToggle
        checked={enabled}
        onChange={setEnabled}
        label="Auto-fire brain on typing pause"
        description="Runs the local brain after 800ms of no typing once the draft is at least 25 characters and suggests @-tokens above the composer."
      />
    </SettingsSection>
  );
}

// The start/stop toggle is not probed from the backend; remember what the user
// did last across section mounts so re-opening the tab doesn't read "stopped"
// while monitors are still running.
let monitorsActiveMemo = false;

/** Background monitors defined in `.cortex/monitors/monitors.json`. */
function MonitorsSection() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const root = activeProject?.root;
  const [monitors, setMonitors] = useState<MonitorSpec[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [active, setActive] = useState(monitorsActiveMemo);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!root) {
      setMonitors([]);
      setErr(null);
      return;
    }
    let cancelled = false;
    listMonitors(root)
      .then((list) => {
        if (!cancelled) {
          setMonitors(list);
          setErr(null);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setMonitors([]);
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  async function toggle(next: boolean) {
    if (!root) return;
    setErr(null);
    setBusy(true);
    try {
      if (next) await startMonitors(root);
      else await stopMonitors();
      monitorsActiveMemo = next;
      setActive(next);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsSection
      title="Monitors"
      description={
        <>
          Background commands defined in
          <code> .cortex/monitors/monitors.json </code>
          are tailed and their output is surfaced as synthetic chat messages.
          Toggle below to start/stop the whole set.
        </>
      }
    >
      {!activeProject && (
        <div className="settings-hint">
          Pick a project to configure monitors.
        </div>
      )}
      {activeProject && (
        <div className="settings-stack">
          <SettingsToggle
            checked={active}
            disabled={busy || monitors.length === 0}
            onChange={(next) => void toggle(next)}
            label={
              <>
                {active ? "Monitors running" : "Monitors stopped"}
                {busy && (
                  <small className="settings-muted gap-left">(working…)</small>
                )}
              </>
            }
            description="Starts or stops every monitor below. Output arrives in the chat as system notes at each monitor's level."
          />
          {err && <div className="settings-err">{err}</div>}
          {monitors.length === 0 ? (
            <div className="settings-hint">
              No monitors in
              <code> {activeProject.root}/.cortex/monitors/monitors.json</code>.
              Drop a JSON array there to enable.
            </div>
          ) : (
            <ul className="settings-list">
              {monitors.map((m) => (
                <li key={m.name} className="settings-list-row mono">
                  <strong>{m.name}</strong>
                  <span className="settings-muted">
                    {m.command} {m.args.join(" ")}
                  </span>
                  <span className="settings-microlabel">{m.level}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </SettingsSection>
  );
}

export const WORKSPACE_SECTIONS: SectionDef[] = [
  {
    tab: "workspace",
    heading: "Obsidian vault",
    text: "obsidian vault path workspace notes brain cortex per-project",
    render: (ctx) => <ObsidianSection gw={ctx.gateway} />,
  },
  {
    tab: "workspace",
    heading: "Import chat history",
    text: "import chat history claude chatgpt openai export json session token pull migrate conversations recent sessions experimental",
    render: () => <ImportSettings />,
  },
  {
    tab: "workspace",
    heading: "History sync",
    text: "history sync auto automatic chat claude chatgpt browser cookie session login connect sign in schedule background pull web app conversations keep updated toggle",
    render: () => <HistorySyncSection />,
  },
  {
    tab: "workspace",
    heading: "Project rules",
    text: "rules cortex mdc cursor activation globs description manual always apply frontmatter",
    render: () => <ProjectRulesSection />,
  },
  {
    tab: "workspace",
    heading: "Brain",
    text: "brain auto context massive memory grep recent fragments @-tokens disable",
    render: () => <BrainSettingsSection />,
  },
  {
    tab: "workspace",
    heading: "Monitors",
    text: "monitors background tail watch tests logs commands processes spawn child npm error info warn",
    render: () => <MonitorsSection />,
  },
];
