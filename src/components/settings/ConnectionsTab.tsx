import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { applyProfile, listProfiles, type Profile } from "@/lib/profiles";
import {
  DEFAULT_SANDBOX_TIER,
  SANDBOX_TIERS,
  SANDBOX_TIER_META,
  getSandboxTier,
  setSandboxTier,
  type SandboxTier,
} from "@/lib/sandbox";
import {
  listAutoApprove,
  removeAutoApprove,
  type AutoApproveEntry,
} from "@/lib/approvals";
import McpServerSettings from "../McpServerSettings";
import { SettingsSection } from "./Section";
import { SafetySection } from "./SafetySection";
import { TailscaleSection } from "./TailscaleSection";
import type { GatewayForm, SectionDef } from "./types";

/** Gateway URL / model / bearer key — committed by the footer Save button. */
function GatewaySection({ gw }: { gw: GatewayForm }) {
  return (
    <SettingsSection title="Gateway backend">
      <label>
        Gateway backend URL
        <input
          value={gw.baseUrl}
          onChange={(e) => gw.setBaseUrl(e.target.value)}
        />
      </label>
      <label>
        Model id
        <input value={gw.model} onChange={(e) => gw.setModel(e.target.value)} />
      </label>
      <label>
        Gateway API key{" "}
        {gw.hasKey && (
          <small className="settings-success">
            (configured — leave blank to keep)
          </small>
        )}
        <input
          type="password"
          value={gw.apiKey}
          onChange={(e) => gw.setApiKey(e.target.value)}
          placeholder={
            gw.hasKey
              ? "leave blank to keep current"
              : "Bearer key for /v1/* access"
          }
        />
      </label>
    </SettingsSection>
  );
}

function OllamaSection({ gw }: { gw: GatewayForm }) {
  return (
    <SettingsSection title="Ollama">
      <label>
        Ollama base URL
        <input
          value={gw.ollamaUrl}
          onChange={(e) => gw.setOllamaUrl(e.target.value)}
        />
      </label>
      <label>
        Ollama model
        <input
          value={gw.ollamaModel}
          onChange={(e) => gw.setOllamaModel(e.target.value)}
        />
      </label>
    </SettingsSection>
  );
}

/**
 * Per-project sandbox tier picker. Loads the tier the chat.rs gate is
 * currently enforcing for the active project when the section mounts.
 */
function SandboxTierSection() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const root = activeProject?.root;
  const [tier, setTier] = useState<SandboxTier>(DEFAULT_SANDBOX_TIER);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!root) {
      setTier(DEFAULT_SANDBOX_TIER);
      return;
    }
    let cancelled = false;
    getSandboxTier(root)
      .then((t) => {
        if (!cancelled) {
          setTier(t);
          setErr(null);
        }
      })
      .catch((e) => {
        if (!cancelled) setErr(humanizeError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  async function pick(next: SandboxTier) {
    if (!root || next === tier) return;
    setErr(null);
    try {
      await setSandboxTier(root, next);
      setTier(next);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  return (
    <SettingsSection title="Sandbox tier">
      {!activeProject ? (
        <div className="settings-hint">
          Pick a project to configure <code>.cortex/sandbox.toml</code>.
        </div>
      ) : (
        <div className="settings-stack">
          {SANDBOX_TIERS.map((t) => {
            const m = SANDBOX_TIER_META[t];
            return (
              <label
                key={t}
                className={`sandbox-radio${t === tier ? " selected" : ""}`}
                style={t === tier ? { borderColor: m.color } : undefined}
              >
                <input
                  type="radio"
                  name="settings-sandbox-tier"
                  checked={t === tier}
                  onChange={() => void pick(t)}
                />
                <span className="sandbox-radio-body">
                  <span
                    className="sandbox-radio-label"
                    style={{ color: m.color }}
                  >
                    {m.label}
                  </span>
                  <small className="sandbox-radio-desc">{m.description}</small>
                </span>
              </label>
            );
          })}
          {err && <div className="settings-err">{err}</div>}
          <div className="settings-hint">
            Tier rejections are deny-bias and override approval rules. High-risk
            guardrails still apply on top.
          </div>
        </div>
      )}
    </SettingsSection>
  );
}

/**
 * "Always-allow grants" — the revoke list for the global auto-approve allowlist
 * (`~/.cortex/auto-approve.json`). Every "Always allow" the user clicks in an
 * approval prompt lands here as a persistent, global-scope grant; this section
 * lets them review and revoke them. Loaded on open and after each revoke so the
 * list stays in sync with the on-disk file.
 */
function AutoApproveSection() {
  const [rows, setRows] = useState<AutoApproveEntry[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);

  async function refresh() {
    try {
      setRows(await listAutoApprove());
      setErr(null);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function revoke(index: number) {
    setBusy(index);
    setErr(null);
    try {
      await removeAutoApprove(index);
      await refresh();
      pushToast({ title: "Grant revoked", kind: "success" });
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <SettingsSection
      title="Always-allow grants"
      description={
        <>
          Tools you've granted a permanent, global "Always allow" via an
          approval prompt. These live in{" "}
          <code>~/.cortex/auto-approve.json</code> and skip the approval step on
          every project. Revoke any you no longer trust.
        </>
      }
    >
      {err && <div className="settings-err">{err}</div>}
      {rows && rows.length === 0 && (
        <div className="settings-hint">
          No always-allow grants. You haven't permanently auto-approved any
          tools.
        </div>
      )}
      {rows && rows.length > 0 && (
        <ul className="settings-list">
          {rows.map((r, i) => (
            <li
              key={`${r.tool}|${r.pattern}|${i}`}
              className="settings-list-row"
            >
              <code>{r.tool.trim() === "" ? "(any tool)" : r.tool}</code>
              <span className="settings-muted settings-mono">{r.pattern}</span>
              {r.profile && (
                <span className="settings-microlabel">{r.profile}</span>
              )}
              <button
                type="button"
                className="settings-label-sm danger"
                onClick={() => void revoke(i)}
                disabled={busy !== null}
              >
                {busy === i ? "Revoking…" : "Revoke"}
              </button>
            </li>
          ))}
        </ul>
      )}
    </SettingsSection>
  );
}

/** Profile bundles (`.cortex/profiles/*.toml`) for the active project. */
function ProfileSection() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const currentProfile = useCortexStore((s) => s.currentProfile);
  const setCurrentProfile = useCortexStore((s) => s.setCurrentProfile);
  const root = activeProject?.root;
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [err, setErr] = useState<string | null>(null);

  // Cheap (single fs scan) and avoids a stale list after the user edits a
  // TOML on disk between visits.
  useEffect(() => {
    if (!root) {
      setProfiles([]);
      return;
    }
    let cancelled = false;
    listProfiles(root)
      .then((list) => {
        if (!cancelled) setProfiles(list);
      })
      .catch((e) => {
        if (!cancelled) {
          setProfiles([]);
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  async function switchProfile(name: string) {
    if (!root) return;
    setErr(null);
    try {
      const applied = await applyProfile(root, name);
      setCurrentProfile(applied);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  return (
    <SettingsSection title="Profile">
      {!activeProject && (
        <div className="settings-hint">
          Pick a project to load its <code>.cortex/profiles/*.toml</code>{" "}
          bundles.
        </div>
      )}
      {activeProject && (
        <div className="settings-stack">
          <div className="settings-hint">
            Active:{" "}
            <strong className="settings-emph">
              {currentProfile?.name ?? "none"}
            </strong>
          </div>
          {currentProfile && (
            <div className="settings-hint">
              {currentProfile.model && (
                <>
                  model: <code>{currentProfile.model}</code>
                  <br />
                </>
              )}
              {currentProfile.sandbox_tier && (
                <>
                  sandbox: <code>{currentProfile.sandbox_tier}</code>
                  <br />
                </>
              )}
              {currentProfile.reasoning_effort && (
                <>
                  reasoning: <code>{currentProfile.reasoning_effort}</code>
                  <br />
                </>
              )}
              {currentProfile.allowed_tools &&
                currentProfile.allowed_tools.length > 0 && (
                  <>
                    tools:{" "}
                    <code>{currentProfile.allowed_tools.join(", ")}</code>
                    <br />
                  </>
                )}
              {currentProfile.system_prompt && (
                <>
                  prompt:{" "}
                  <code>
                    {currentProfile.system_prompt.slice(0, 80)}
                    {currentProfile.system_prompt.length > 80 ? "…" : ""}
                  </code>
                </>
              )}
            </div>
          )}
          {profiles.length === 0 ? (
            <div className="settings-hint">
              No profiles in <code>{activeProject.root}/.cortex/profiles/</code>
              . Drop a <code>&lt;name&gt;.toml</code> there to enable switching.
            </div>
          ) : (
            <div className="settings-row wrap">
              {profiles.map((p) => (
                <button
                  key={p.name}
                  type="button"
                  onClick={() => void switchProfile(p.name)}
                  disabled={currentProfile?.name === p.name}
                  title={p.system_prompt ?? p.model ?? p.name}
                >
                  {p.name}
                </button>
              ))}
            </div>
          )}
          {err && <div className="settings-err">{err}</div>}
        </div>
      )}
    </SettingsSection>
  );
}

function SafetySectionForProject() {
  const root = useCortexStore((s) => s.activeProject?.root ?? null);
  return <SafetySection projectRoot={root} />;
}

export const CONNECTIONS_SECTIONS: SectionDef[] = [
  {
    tab: "connections",
    heading: "Gateway backend",
    text: "gateway backend url model id api key bearer v1",
    render: (ctx) => <GatewaySection gw={ctx.gateway} />,
  },
  {
    tab: "connections",
    heading: "Sandbox tier",
    text: "sandbox tier read-only workspace-write danger full access codex three permission gate",
    render: () => <SandboxTierSection />,
  },
  {
    tab: "connections",
    heading: "Always-allow grants",
    text: "always allow grants auto approve allowlist revoke remove permanent global scope auto-approve.json shell file destructive permission tool pattern trust",
    render: () => <AutoApproveSection />,
  },
  {
    tab: "connections",
    heading: "Safety",
    text: "safety safe mode lockdown policy pack shield command policy allowlist denylist deny allow ask audit log export jsonl csv dry run test command redact command-policy.toml safe-mode.json",
    render: () => <SafetySectionForProject />,
  },
  {
    tab: "connections",
    heading: "Profile",
    text: "profile bundle model sandbox reasoning allowed tools toml cortex switch active",
    render: () => <ProfileSection />,
  },
  {
    tab: "connections",
    heading: "Ollama",
    text: "ollama base url model local llm",
    render: (ctx) => <OllamaSection gw={ctx.gateway} />,
  },
  {
    tab: "connections",
    heading: "Tailscale (embedded)",
    text: "tailscale embedded userspace tsnet socks5 proxy tailnet magicdns vpn mesh remote home gateway login authkey auth key connect network connectivity no admin",
    render: () => <TailscaleSection />,
  },
  {
    tab: "connections",
    heading: "Cortex as MCP server",
    text: "mcp server model context protocol claude code codex gemini cli brain checkpoints bearer token external agents terminal",
    render: () => <McpServerSettings />,
  },
];
