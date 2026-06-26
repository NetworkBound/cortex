/**
 * PRP (Product Requirement Prompt) panel — staged feature-spec view backed by
 * `<project_root>/.cortex/prps/<name>.md` files. Top of the panel shows a
 * compact list with stage badge + per-gate pills; clicking expands the
 * goal/gotchas/acceptance body and exposes "Advance stage" / "Run gates"
 * actions.
 *
 * Empty / no-project states are explicit so the panel never just shows a
 * silent blank pane — picking a project from the sidebar (or running
 * `/prp create <name>`) is the obvious next action.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  advancePrpStage,
  createPrp,
  listPrps,
  runPrpGates,
  stageLabel,
  stageOrdinal,
  type GateResult,
  type Prp,
} from "@/lib/prp";
import { useCortexStore } from "@/state/store";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";

type Status = { kind: "idle" | "info" | "success" | "error"; text: string };

const GATE_ORDER = ["syntax", "tests", "coverage", "build", "security"] as const;

export function PRPPanel() {
  const project = useCortexStore((s) => s.activeProject);
  const projectRoot = project?.root ?? null;

  const [prps, setPrps] = useState<Prp[] | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [status, setStatus] = useState<Status>({ kind: "idle", text: "" });
  const [busyName, setBusyName] = useState<string | null>(null);
  const [lastReport, setLastReport] = useState<Record<string, GateResult[]>>({});
  const [loadError, setLoadError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!projectRoot) {
      setPrps([]);
      return;
    }
    try {
      const list = await listPrps(projectRoot);
      setLoadError(null);
      setPrps(list);
      setExpanded((prev) => (prev && list.some((p) => p.name === prev) ? prev : list[0]?.name ?? null));
    } catch (err) {
      // Don't leave the panel stuck on the loading skeleton forever — surface
      // the failure with a retry path instead.
      setLoadError(humanizeError(err));
      setPrps([]);
    }
  }, [projectRoot]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const expandedPrp = useMemo(
    () => prps?.find((p) => p.name === expanded) ?? null,
    [prps, expanded],
  );

  async function handleCreate() {
    const name = newName.trim();
    if (!name || !projectRoot) return;
    try {
      const prp = await createPrp(projectRoot, name);
      setNewName("");
      setStatus({ kind: "success", text: `Created PRP '${prp.name}'` });
      await reload();
      setExpanded(prp.name);
    } catch (err) {
      setStatus({ kind: "error", text: humanizeError(err) });
    }
  }

  async function handleAdvance(prp: Prp) {
    if (!projectRoot) return;
    setBusyName(prp.name);
    try {
      await advancePrpStage(projectRoot, prp.name);
      setStatus({ kind: "success", text: `Advanced '${prp.name}'` });
      await reload();
    } catch (err) {
      setStatus({ kind: "error", text: humanizeError(err) });
    } finally {
      setBusyName(null);
    }
  }

  async function handleRunGates(prp: Prp) {
    if (!projectRoot) return;
    setBusyName(prp.name);
    setStatus({ kind: "info", text: `Running gates for '${prp.name}'…` });
    try {
      const report = await runPrpGates(projectRoot, prp.name);
      setLastReport((prev) => ({ ...prev, [prp.name]: report.gates }));
      const failed = report.gates.filter((g) => g.verdict === "fail").length;
      setStatus(
        failed === 0
          ? { kind: "success", text: `All gates resolved for '${prp.name}'` }
          : { kind: "error", text: `${failed} gate${failed === 1 ? "" : "s"} failed` },
      );
      await reload();
    } catch (err) {
      setStatus({ kind: "error", text: humanizeError(err) });
    } finally {
      setBusyName(null);
    }
  }

  if (!projectRoot) {
    return (
      <div className="muted prp-empty">
        Pick a project from the sidebar to manage PRPs.
      </div>
    );
  }

  if (loadError) {
    return (
      <div className="muted prp-empty">
        <div className="prp-status prp-status-error">{loadError}</div>
        <button
          type="button"
          className="prp-btn"
          onClick={() => {
            setPrps(null);
            void reload();
          }}
        >
          Retry
        </button>
      </div>
    );
  }

  if (prps === null) {
    return <PanelLoading label="Loading PRPs" />;
  }

  return (
    <div className="prp-panel">
      <div className="prp-create">
        <input
          type="text"
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          placeholder="new PRP slug (e.g. add-redis-cache)"
          className="prp-create-input"
          onKeyDown={(e) => {
            if (e.key === "Enter") void handleCreate();
          }}
        />
        <button
          type="button"
          className="prp-btn prp-btn-primary"
          onClick={() => void handleCreate()}
          disabled={!newName.trim()}
        >
          + Create
        </button>
      </div>

      {status.text && (
        <div className={`prp-status prp-status-${status.kind}`}>{status.text}</div>
      )}

      {prps.length === 0 ? (
        <div className="muted prp-empty">
          No PRPs yet. Create one above or run <code>/prp create &lt;name&gt;</code>.
        </div>
      ) : (
        <ul className="prp-list">
          {prps.map((p) => {
            const isOpen = p.name === expanded;
            const liveGates = lastReport[p.name];
            return (
              <li key={p.name} className={`prp-item ${isOpen ? "open" : ""}`}>
                <button
                  type="button"
                  className="prp-row"
                  onClick={() => setExpanded(isOpen ? null : p.name)}
                  aria-expanded={isOpen}
                >
                  <span className="prp-row-name">{p.name}</span>
                  <span className="prp-stage-badge" title={stageLabel(p.status)}>
                    {stageOrdinal(p.status)} · {stageLabel(p.status)}
                  </span>
                  <span className="prp-gates-strip">
                    {GATE_ORDER.map((g) => {
                      const v = (p.gates[g] as string | undefined) ?? "pending";
                      return (
                        <span
                          key={g}
                          className={`prp-gate prp-gate-${v}`}
                          title={`${g}: ${v}`}
                        >
                          {g[0].toUpperCase()}
                        </span>
                      );
                    })}
                  </span>
                </button>

                {isOpen && expandedPrp && expandedPrp.name === p.name && (
                  <div className="prp-detail">
                    <pre className="prp-body">{expandedPrp.body}</pre>

                    <div className="prp-gates-table">
                      {GATE_ORDER.map((name) => {
                        const live = liveGates?.find((r) => r.name === name);
                        const verdict = live?.verdict ?? (p.gates[name] as string | undefined) ?? "pending";
                        return (
                          <div className="prp-gate-row" key={name}>
                            <span className={`prp-gate-pill prp-gate-${verdict}`}>
                              {verdict}
                            </span>
                            <span className="prp-gate-name">{name}</span>
                            {live?.message && (
                              <span className="prp-gate-msg muted">{live.message}</span>
                            )}
                          </div>
                        );
                      })}
                    </div>

                    <div className="prp-actions">
                      <button
                        type="button"
                        className="prp-btn"
                        onClick={() => void handleRunGates(p)}
                        disabled={busyName === p.name}
                      >
                        {busyName === p.name ? "Running…" : "Run gates"}
                      </button>
                      <button
                        type="button"
                        className="prp-btn prp-btn-primary"
                        onClick={() => void handleAdvance(p)}
                        disabled={busyName === p.name || p.status === "stage-4"}
                        title={p.status === "stage-4" ? "Already at final stage" : "Advance to next stage"}
                      >
                        Advance stage
                      </button>
                      <span className="muted prp-path">{p.path}</span>
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
