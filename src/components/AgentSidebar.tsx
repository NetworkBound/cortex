import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, RefreshCw, SquarePen } from "lucide-react";
import { checkAgentHealth, listAgents } from "@/lib/cortex-bridge";
import { humanizeError } from "@/lib/errors";
import { useCortexStore } from "@/state/store";
import { AgentInstructionsEditor } from "./AgentInstructionsEditor";
import { RolesPanel } from "./RolesPanel";

type LoadState = "loading" | "ready" | "error";

/** Live status per agent id. `checking` only shows before the FIRST result
 *  lands — re-checks keep the previous verdict so the dots never flicker. */
type Health = "checking" | "ok" | "down";

const HEALTH_TITLES: Record<Health, string> = {
  checking: "Checking…",
  ok: "Online — responded to a live health check",
  down: "Unreachable — the last health check failed",
};

export function AgentSidebar() {
  const agents = useCortexStore((s) => s.agents);
  const setAgents = useCortexStore((s) => s.setAgents);
  // Tracks which agent (by id) is being edited; `null` means the modal is
  // closed. We capture the label too so the modal header stays meaningful
  // even if the agent list refreshes mid-edit.
  const [editing, setEditing] = useState<{ id: string; label: string } | null>(
    null,
  );
  // Collapsible "Roles" section sits ABOVE the existing agent list. Closed
  // by default to keep the sidebar lean for users who don't use personas.
  const [rolesOpen, setRolesOpen] = useState(false);
  const [loadState, setLoadState] = useState<LoadState>(
    () => (useCortexStore.getState().agents.length > 0 ? "ready" : "loading"),
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [health, setHealth] = useState<Record<string, Health>>({});
  const [checking, setChecking] = useState(false);
  const mountedRef = useRef(true);

  // Each agent's status dot comes from a REAL `check_agent_health` round trip
  // (every adapter's probe is bounded at ~3s), not the static `available`
  // flag from the descriptor — so "online" means it answered just now.
  const checkHealth = useCallback(async (ids: string[]) => {
    if (ids.length === 0) return;
    setChecking(true);
    setHealth((prev) => {
      const next = { ...prev };
      for (const id of ids) if (!next[id]) next[id] = "checking";
      return next;
    });
    const verdicts = await Promise.all(
      ids.map(async (id): Promise<[string, Health]> => {
        try {
          return [id, (await checkAgentHealth(id)) ? "ok" : "down"];
        } catch {
          return [id, "down"];
        }
      }),
    );
    if (!mountedRef.current) return;
    setHealth((prev) => ({ ...prev, ...Object.fromEntries(verdicts) }));
    setChecking(false);
  }, []);

  // Concurrent refreshes share one round trip: a slow fetch overlapping the
  // 30 s interval (or StrictMode's doubled dev mount) must not stack probes.
  const refreshInFlight = useRef<Promise<void> | null>(null);

  const refresh = useCallback(() => {
    if (refreshInFlight.current) return refreshInFlight.current;
    const run = (async () => {
      try {
        const list = await listAgents();
        if (!mountedRef.current) return;
        setAgents(list);
        setLoadState("ready");
        setLoadError(null);
        void checkHealth(list.map((a) => a.id));
      } catch (err) {
        if (!mountedRef.current) return;
        // A failed background poll over an already-populated list keeps the
        // (stale but real) list; the error card is only for an empty pane.
        if (useCortexStore.getState().agents.length === 0) {
          setLoadState("error");
          setLoadError(humanizeError(err));
        }
      } finally {
        refreshInFlight.current = null;
      }
    })();
    refreshInFlight.current = run;
    return run;
  }, [setAgents, checkHealth]);

  useEffect(() => {
    mountedRef.current = true;
    void refresh();
    const t = setInterval(() => void refresh(), 30_000);
    return () => {
      mountedRef.current = false;
      clearInterval(t);
    };
  }, [refresh]);

  const dotClass = (h: Health | undefined) =>
    h === "ok" ? "ok" : h === "down" ? "off" : "checking";

  return (
    <aside className="sidebar agent-sidebar">
      <section className="agent-sidebar-section">
        <button
          type="button"
          className="agent-sidebar-section-toggle"
          aria-expanded={rolesOpen}
          onClick={() => setRolesOpen((v) => !v)}
        >
          <span className="agent-sidebar-section-caret">
            {rolesOpen ? (
              <ChevronDown size={14} strokeWidth={1.75} />
            ) : (
              <ChevronRight size={14} strokeWidth={1.75} />
            )}
          </span>
          <h2>Roles</h2>
        </button>
        {rolesOpen && (
          <RolesPanel agents={agents} defaultAgentId={agents[0]?.id ?? null} />
        )}
      </section>

      <h2 className="agent-sidebar-heading">Agents</h2>

      {loadState === "loading" && (
        <div className="muted agent-sidebar-status" role="status">
          Loading agents…
        </div>
      )}

      {loadState === "error" && (
        <div className="agent-error-state" role="alert">
          <div className="agent-error-state-title">Couldn't load agents</div>
          <p className="agent-error-state-hint">{loadError}</p>
          <button
            type="button"
            className="link-btn"
            onClick={() => {
              setLoadState("loading");
              setLoadError(null);
              void refresh();
            }}
          >
            Retry
          </button>
        </div>
      )}

      {loadState === "ready" && agents.length === 0 && (
        <div className="muted agent-sidebar-status">
          No agents are registered yet. Configure a provider in Settings, or
          pull a local model from the Cookbook.
        </div>
      )}

      {agents.map((a) => (
        <div key={a.id} className="agent-row">
          <div className="agent-row-head">
            <strong>{a.label}</strong>
            <span className="agent-row-actions">
              <button
                type="button"
                className="agent-row-edit"
                title="Edit custom instructions"
                aria-label={`Edit instructions for ${a.label}`}
                onClick={() => setEditing({ id: a.id, label: a.label })}
              >
                <SquarePen size={13} strokeWidth={1.75} />
              </button>
              <span
                className={`dot ${dotClass(health[a.id])}`}
                title={HEALTH_TITLES[health[a.id] ?? "checking"]}
              />
            </span>
          </div>
          <div className="muted">{a.description}</div>
          <div className="caps">
            {a.capabilities.map((c) => (
              <span key={c} className="cap">{c}</span>
            ))}
          </div>
        </div>
      ))}

      {loadState === "ready" && agents.length > 0 && (
        <div className="muted agent-health-note">
          <span>Status from live health checks, refreshed every 30 s.</span>
          <button
            type="button"
            className="link-btn agent-health-refresh"
            disabled={checking}
            onClick={() => void checkHealth(agents.map((a) => a.id))}
          >
            <RefreshCw
              size={11}
              strokeWidth={1.75}
              className={checking ? "spin" : undefined}
            />
            {checking ? "Checking…" : "Check now"}
          </button>
        </div>
      )}

      {editing && (
        <AgentInstructionsEditor
          agentId={editing.id}
          agentLabel={editing.label}
          onClose={() => setEditing(null)}
        />
      )}
    </aside>
  );
}
