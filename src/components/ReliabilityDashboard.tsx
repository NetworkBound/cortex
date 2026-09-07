import { useEffect, useMemo, useState } from "react";
import {
  reliabilitySummary,
  rowsToCsv,
  type McpToolsSummary,
  type ReliabilityReport,
  type ReliabilityRow,
} from "@/lib/reliability";

/** Time windows offered by the range selector (hours; 0 = all time). */
const WINDOWS: { label: string; hours: number }[] = [
  { label: "24h", hours: 24 },
  { label: "7d", hours: 168 },
  { label: "30d", hours: 720 },
  { label: "All", hours: 0 },
];

type Load = "loading" | "ok" | "error";

/**
 * Agent Reliability Dashboard — success rate, latency percentiles, tokens and
 * estimated cost per provider and per model, aggregated from the local trace
 * store. Read-only. A failing provider/model row deep-links into Run Replay via
 * `onOpenRun` when provided.
 */
export function ReliabilityDashboard({
  onOpenRun,
}: {
  onOpenRun?: (row: ReliabilityRow) => void;
}) {
  const [report, setReport] = useState<ReliabilityReport | null>(null);
  const [state, setState] = useState<Load>("loading");
  const [err, setErr] = useState<string | null>(null);
  const [windowHours, setWindowHours] = useState<number>(168);

  useEffect(() => {
    let mounted = true;
    setState("loading");
    reliabilitySummary(windowHours || undefined)
      .then((r) => {
        if (!mounted) return;
        setReport(r);
        setState("ok");
      })
      .catch((e) => {
        if (!mounted) return;
        setErr(String(e));
        setState("error");
      });
    return () => {
      mounted = false;
    };
  }, [windowHours]);

  const isEmpty = state === "ok" && (report?.totals.runs ?? 0) === 0;

  const download = (name: string, mime: string, body: string) => {
    const blob = new Blob([body], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.click();
    URL.revokeObjectURL(url);
  };

  const exportJson = () => {
    if (!report) return;
    download(
      `cortex-reliability-${Date.now()}.json`,
      "application/json",
      JSON.stringify(report, null, 2),
    );
  };
  const exportCsv = () => {
    if (!report) return;
    const csv =
      "# providers\n" +
      rowsToCsv(report.by_provider) +
      "\n\n# models\n" +
      rowsToCsv(report.by_model);
    download(`cortex-reliability-${Date.now()}.csv`, "text/csv", csv);
  };

  return (
    <div className="reliability">
      <div className="reliability-toolbar">
        <div className="reliability-range" role="tablist" aria-label="Time range">
          {WINDOWS.map((w) => (
            <button
              key={w.hours}
              type="button"
              role="tab"
              aria-selected={windowHours === w.hours}
              className={windowHours === w.hours ? "active" : ""}
              onClick={() => setWindowHours(w.hours)}
            >
              {w.label}
            </button>
          ))}
        </div>
        <div className="reliability-actions">
          <button type="button" disabled={!report || isEmpty} onClick={exportCsv}>
            Export CSV
          </button>
          <button type="button" disabled={!report || isEmpty} onClick={exportJson}>
            Export JSON
          </button>
        </div>
      </div>

      <p className="reliability-note muted">
        Local view aggregated from on-device run traces. “Success” is derived
        from run status + error events; gateway-internal retries are not visible,
        and cost is an estimate (50/50 token split, prefix pricing).
      </p>

      {state === "loading" && <div className="reliability-empty">Loading reliability…</div>}
      {state === "error" && (
        <div className="reliability-empty error">Couldn’t load reliability: {err}</div>
      )}
      {isEmpty && (
        <div className="reliability-empty">
          No runs recorded in this window yet. Send a chat or run an agent to
          populate the dashboard.
        </div>
      )}

      {state === "ok" && !isEmpty && report && (
        <>
          <SummaryCards totals={report.totals} />
          <RowTable
            title="By provider"
            rows={report.by_provider}
            keyLabel="Provider"
            onOpenRun={onOpenRun}
          />
          <RowTable
            title="By model"
            rows={report.by_model}
            keyLabel="Model"
            onOpenRun={onOpenRun}
          />
          <McpToolsSection mcp={report.mcp_tools} />
        </>
      )}
    </div>
  );
}

/**
 * MCP tools section (issue 008 full scope): per-tool call count, success
 * rate, and avg/p95 latency, plus a run-level cost estimate — aggregated
 * server-side from the `tool_call`/`tool_result` events every model-initiated
 * MCP call already emits. Quietly notes "no activity" rather than disappearing
 * outright, since MCP-in-chat is default-off and a user checking this section
 * for the first time should see that it's wired up, not wonder if it's missing.
 */
function McpToolsSection({ mcp }: { mcp: McpToolsSummary }) {
  const sorted = useMemo(() => [...mcp.by_tool].sort((a, b) => b.calls - a.calls), [mcp.by_tool]);
  return (
    <div className="reliability-table-wrap">
      <h4>MCP tools</h4>
      {mcp.calls === 0 ? (
        <p className="reliability-empty-inline muted">
          No MCP tool calls in this window. Expose a server's tools to chat
          from the MCP panel to see call volume, latency, and cost here.
        </p>
      ) : (
        <>
          <div className="reliability-cards reliability-cards-compact">
            <Card label="Tool calls" value={String(mcp.calls)} sub={`${mcp.calls - mcp.ok_calls} failed`} />
            <Card label="p95 latency" value={fmtMs(mcp.p95_ms)} sub={`avg ${fmtMs(mcp.avg_ms)}`} />
            <Card label="Est. cost" value={fmtUsd(mcp.est_usd)} sub="runs that used MCP tools" />
          </div>
          <table className="reliability-table">
            <thead>
              <tr>
                <th>Tool</th>
                <th>Calls</th>
                <th>Success</th>
                <th>Avg</th>
                <th>p95</th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((t) => {
                const rate = t.calls > 0 ? t.ok_calls / t.calls : 0;
                const pct = (rate * 100).toFixed(rate >= 0.995 ? 0 : 1);
                return (
                  <tr key={t.name} className={t.ok_calls < t.calls ? "has-errors" : ""}>
                    <td className="reliability-key" title={t.name}>
                      {t.name}
                    </td>
                    <td>{t.calls}</td>
                    <td className={rate < 0.7 ? "bad" : rate < 0.9 ? "warn" : "good"}>{pct}%</td>
                    <td>{fmtMs(t.avg_ms)}</td>
                    <td>{fmtMs(t.p95_ms)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </>
      )}
    </div>
  );
}

function SummaryCards({ totals }: { totals: ReliabilityRow }) {
  const pct = (totals.success_rate * 100).toFixed(totals.success_rate >= 0.995 ? 0 : 1);
  return (
    <div className="reliability-cards">
      <Card label="Runs" value={String(totals.runs)} sub={`${totals.error_runs} failed`} />
      <Card
        label="Success"
        value={`${pct}%`}
        sub={`${totals.ok_runs}/${totals.ok_runs + totals.error_runs} finished`}
        tone={totals.success_rate >= 0.9 ? "good" : totals.success_rate >= 0.7 ? "warn" : "bad"}
      />
      <Card label="p95 latency" value={fmtMs(totals.p95_ms)} sub={`p50 ${fmtMs(totals.p50_ms)}`} />
      <Card label="Est. cost" value={fmtUsd(totals.est_usd)} sub={`${fmtTokens(totals.total_tokens)} tok`} />
    </div>
  );
}

function Card({
  label,
  value,
  sub,
  tone,
}: {
  label: string;
  value: string;
  sub?: string;
  tone?: "good" | "warn" | "bad";
}) {
  return (
    <div className={`reliability-card${tone ? ` tone-${tone}` : ""}`}>
      <div className="reliability-card-value">{value}</div>
      <div className="reliability-card-label">{label}</div>
      {sub && <div className="reliability-card-sub muted">{sub}</div>}
    </div>
  );
}

function RowTable({
  title,
  rows,
  keyLabel,
  onOpenRun,
}: {
  title: string;
  rows: ReliabilityRow[];
  keyLabel: string;
  onOpenRun?: (row: ReliabilityRow) => void;
}) {
  const sorted = useMemo(() => [...rows].sort((a, b) => b.runs - a.runs), [rows]);
  if (sorted.length === 0) return null;
  return (
    <div className="reliability-table-wrap">
      <h4>{title}</h4>
      <table className="reliability-table">
        <thead>
          <tr>
            <th>{keyLabel}</th>
            <th>Runs</th>
            <th>Success</th>
            <th>p50</th>
            <th>p95</th>
            <th>Tokens</th>
            <th>Est&nbsp;$</th>
            <th>Top error</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((r) => {
            const pct = (r.success_rate * 100).toFixed(r.success_rate >= 0.995 ? 0 : 1);
            const failing = r.error_runs > 0;
            return (
              <tr key={r.key} className={failing ? "has-errors" : ""}>
                <td className="reliability-key" title={r.key}>
                  {r.key}
                </td>
                <td>{r.runs}</td>
                <td className={r.success_rate < 0.7 ? "bad" : r.success_rate < 0.9 ? "warn" : "good"}>
                  {pct}%
                </td>
                <td>{fmtMs(r.p50_ms)}</td>
                <td>{fmtMs(r.p95_ms)}</td>
                <td>{fmtTokens(r.total_tokens)}</td>
                <td>{fmtUsd(r.est_usd)}</td>
                <td className="reliability-err">
                  {r.top_error_class ? (
                    onOpenRun && failing ? (
                      <button
                        type="button"
                        className="reliability-replay-link"
                        title="Open the most recent failing run in Run Replay"
                        onClick={() => onOpenRun(r)}
                      >
                        {r.top_error_class} ↗
                      </button>
                    ) : (
                      <span>{r.top_error_class}</span>
                    )
                  ) : (
                    <span className="muted">—</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function fmtMs(ms: number | null): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
}
function fmtUsd(usd: number): string {
  if (usd <= 0) return "$0";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(2)}`;
}
function fmtTokens(t: number): string {
  if (t < 1000) return String(t);
  if (t < 1_000_000) return `${(t / 1000).toFixed(1)}k`;
  return `${(t / 1_000_000).toFixed(2)}M`;
}
