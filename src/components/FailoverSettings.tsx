import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { listAgents, type AgentDescriptor } from "@/lib/cortex-bridge";

/**
 * Mirrors `FailoverPolicy` in `src-tauri/src/orchestrator/failover.rs`
 * (`~/.cortex/failover.json`). DEFAULT-OFF.
 */
export interface FailoverPolicy {
  enabled: boolean;
  chain: string[];
  threshold_pct: number;
  on_transient_error: boolean;
  on_warning: boolean;
}

export async function getFailoverPolicy(): Promise<FailoverPolicy> {
  return invoke<FailoverPolicy>("get_failover_policy");
}

export async function setFailoverPolicy(
  policy: FailoverPolicy,
): Promise<FailoverPolicy> {
  return invoke<FailoverPolicy>("set_failover_policy", { policy });
}

/**
 * "Quota-aware failover" — Settings → Providers. When the picked agent is
 * out of quota (Claude's own rate-limit status) or a run dies with a
 * rate-limit / transient error before streaming anything, the same turn is
 * sent once more to the next agent in this ordered chain. The backend file
 * is the source of truth; every change is written immediately.
 */
export function FailoverSection() {
  const [policy, setPolicy] = useState<FailoverPolicy | null>(null); // null = loading
  const [agents, setAgents] = useState<AgentDescriptor[]>([]);
  const [chainText, setChainText] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void getFailoverPolicy()
      .then((p) => {
        if (cancelled) return;
        setPolicy(p);
        setChainText(p.chain.join(", "));
      })
      .catch((e) => {
        if (!cancelled) setErr(humanizeError(e));
      });
    void listAgents()
      .then((a) => {
        if (!cancelled) setAgents(a);
      })
      .catch(() => {
        /* the picker is a convenience; the text field still works */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function save(next: FailoverPolicy) {
    setBusy(true);
    setErr(null);
    try {
      const written = await setFailoverPolicy(next);
      setPolicy(written);
      setChainText(written.chain.join(", "));
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  function parseChain(text: string): string[] {
    return text
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }

  function addAgent(id: string) {
    if (!policy || !id) return;
    const chain = parseChain(chainText);
    if (chain.includes(id)) return;
    void save({ ...policy, chain: [...chain, id] });
  }

  return (
    <div className="settings-section">
      <h3>Quota-aware failover</h3>
      <div className="settings-hint spaced">
        When the picked agent reports its quota exhausted, or a run fails with a
        rate-limit or transient error <em>before</em> streaming any output, send
        the same message once more to the first available agent in the chain
        below. A run that already produced output is never re-sent. The routing
        reason (chat toast and Run Replay) records{" "}
        <code>failed over from … : quota (…)</code>. Off by default; stored in{" "}
        <code>~/.cortex/failover.json</code>.
      </div>
      {policy === null ? (
        <div className="settings-hint">Loading…</div>
      ) : (
        <>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={policy.enabled}
              disabled={busy}
              onChange={(e) =>
                void save({ ...policy, enabled: e.target.checked })
              }
            />
            <span>Fail over chat turns to the next agent in the chain</span>
          </label>
          <label>
            Fallback chain (agent ids, in order)
            <input
              value={chainText}
              disabled={busy}
              placeholder="codex-cli, gemini-cli, ollama"
              onChange={(e) => setChainText(e.target.value)}
              onBlur={() =>
                void save({ ...policy, chain: parseChain(chainText) })
              }
            />
          </label>
          {agents.length > 0 && (
            <label>
              Add an agent
              <select
                value=""
                disabled={busy}
                onChange={(e) => addAgent(e.target.value)}
              >
                <option value="">Pick…</option>
                {agents
                  .filter((a) => a.capabilities.includes("chat"))
                  .map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.label} ({a.id}){a.available ? "" : " — not available"}
                    </option>
                  ))}
              </select>
            </label>
          )}
          <label>
            Usage threshold (%) — fail over pre-emptively at or above
            <input
              type="number"
              min={1}
              max={100}
              step={1}
              value={policy.threshold_pct}
              disabled={busy}
              onChange={(e) => {
                const v = Number(e.target.value);
                if (Number.isFinite(v))
                  setPolicy({ ...policy, threshold_pct: v });
              }}
              onBlur={() => void save(policy)}
            />
          </label>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={policy.on_transient_error}
              disabled={busy}
              onChange={(e) =>
                void save({ ...policy, on_transient_error: e.target.checked })
              }
            />
            <span>Also fail over on other transient errors (5xx, network)</span>
          </label>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={policy.on_warning}
              disabled={busy}
              onChange={(e) =>
                void save({ ...policy, on_warning: e.target.checked })
              }
            />
            <span>
              Treat the provider&apos;s soft usage warning as exhausted
            </span>
          </label>
        </>
      )}
      {err && <div className="settings-err">{err}</div>}
    </div>
  );
}
