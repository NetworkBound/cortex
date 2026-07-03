import { useEffect, useState, type ReactNode } from "react";
import {
  listReplayRuns,
  runReplay,
  exportRunReplay,
  type ReplayRunSummary,
  type RunReplay,
  type ReplayStepRow,
} from "@/lib/observability";

/**
 * Run Replay / Agent Black Box — pick a past run and play it back as the
 * ordered timeline it was (prompt → route reason → tool calls → approvals →
 * edits → errors → done), with a redacted JSONL export. Read-only. When
 * `focusSpanId` is set (e.g. deep-linked from the Reliability dashboard), that
 * run opens immediately.
 */
export function RunReplayView({ focusSpanId }: { focusSpanId?: string | null }) {
  const [runs, setRuns] = useState<ReplayRunSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(focusSpanId ?? null);
  const [detail, setDetail] = useState<RunReplay | null>(null);
  const [listState, setListState] = useState<"loading" | "ok" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let mounted = true;
    listReplayRuns(undefined, 40)
      .then((r) => {
        if (!mounted) return;
        setRuns(r);
        setListState("ok");
        // Default to the deep-linked run, else the newest.
        setSelected((cur) => cur ?? r[0]?.span_id ?? null);
      })
      .catch((e) => {
        if (!mounted) return;
        setErr(String(e));
        setListState("error");
      });
    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    if (focusSpanId) setSelected(focusSpanId);
  }, [focusSpanId]);

  useEffect(() => {
    if (!selected) {
      setDetail(null);
      return;
    }
    let mounted = true;
    setDetail(null);
    runReplay(selected)
      .then((d) => mounted && setDetail(d))
      .catch((e) => mounted && setErr(String(e)));
    return () => {
      mounted = false;
    };
  }, [selected]);

  const doExport = async () => {
    if (!selected) return;
    try {
      const jsonl = await exportRunReplay(selected);
      const blob = new Blob([jsonl], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `cortex-run-${selected.slice(0, 8)}.jsonl`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div className="replay">
      <div className="replay-list">
        {listState === "loading" && <div className="replay-empty">Loading runs…</div>}
        {listState === "error" && <div className="replay-empty error">Couldn’t load runs: {err}</div>}
        {listState === "ok" && runs.length === 0 && (
          <div className="replay-empty">No runs recorded yet. Send a chat to record one.</div>
        )}
        {runs.map((r) => (
          <button
            key={r.span_id}
            type="button"
            className={`replay-run${selected === r.span_id ? " active" : ""}${r.had_error ? " has-error" : ""}`}
            onClick={() => setSelected(r.span_id)}
            title={r.prompt_preview ?? r.span_id}
          >
            <span className={`replay-run-status status-${r.status}`} />
            <span className="replay-run-main">
              <span className="replay-run-title">
                {r.prompt_preview || `${r.agent_id ?? "run"} ${r.span_id.slice(0, 8)}`}
              </span>
              <span className="replay-run-meta muted">
                {r.agent_id ?? "?"}
                {r.model ? ` · ${r.model}` : ""} · {new Date(r.started_at).toLocaleString()}
              </span>
            </span>
          </button>
        ))}
      </div>

      <div className="replay-detail">
        {!selected && <div className="replay-empty">Select a run to replay.</div>}
        {selected && !detail && <div className="replay-empty">Loading timeline…</div>}
        {detail && <ReplayTimeline detail={detail} onExport={doExport} />}
      </div>
    </div>
  );
}

function ReplayTimeline({ detail, onExport }: { detail: RunReplay; onExport: () => void }) {
  const dur = detail.ended_at ? detail.ended_at - detail.started_at : null;
  return (
    <div className="replay-timeline">
      <div className="replay-head">
        <div>
          <div className="replay-head-title">
            {detail.agent_id ?? "run"}
            {detail.model ? ` · ${detail.model}` : ""}
          </div>
          <div className="replay-head-sub muted">
            <span className={`replay-badge status-${detail.status}`}>{detail.status}</span>
            {dur !== null && <span>{fmtMs(dur)}</span>}
            {detail.total_tokens > 0 && <span>{detail.total_tokens} tok</span>}
            {detail.est_usd > 0 && <span>${detail.est_usd.toFixed(2)}</span>}
          </div>
        </div>
        <button type="button" onClick={onExport} title="Export a redacted JSONL of this run">
          Export JSONL
        </button>
      </div>

      {detail.prompt_preview && (
        <div className="replay-step kind-prompt">
          <div className="replay-step-label">Prompt</div>
          <div className="replay-step-body">{detail.prompt_preview}</div>
        </div>
      )}
      {detail.routing_reason && (
        <div className="replay-step kind-route">
          <div className="replay-step-label">Routed</div>
          <div className="replay-step-body muted">{detail.routing_reason}</div>
        </div>
      )}

      {detail.steps.length === 0 && (
        <div className="replay-empty">No events were recorded for this run.</div>
      )}
      {detail.steps.map((s, i) => (
        <Step key={i} step={s} start={detail.started_at} />
      ))}
    </div>
  );
}

function Step({ step, start }: { step: ReplayStepRow; start: number }) {
  const p = step.payload as Record<string, unknown>;
  const at = `+${fmtMs(step.ts - start)}`;
  const label = STEP_LABELS[step.name] ?? step.name;
  return (
    <div className={`replay-step kind-${step.name}`}>
      <div className="replay-step-label">
        {label}
        <span className="replay-step-at muted">{at}</span>
      </div>
      <div className="replay-step-body">{renderPayload(step.name, p)}</div>
    </div>
  );
}

const STEP_LABELS: Record<string, string> = {
  started: "Started",
  token: "Streaming",
  reasoning: "Reasoning",
  tool_call: "Tool call",
  tool_result: "Tool result",
  file_edit: "File edit",
  approval_request: "Approval requested",
  approval_resolved: "Approval resolved",
  error: "Error",
  done: "Done",
};

function renderPayload(name: string, p: Record<string, unknown>): ReactNode {
  const str = (k: string) => (typeof p[k] === "string" ? (p[k] as string) : undefined);
  const num = (k: string) => (typeof p[k] === "number" ? (p[k] as number) : undefined);
  switch (name) {
    case "tool_call":
      return (
        <span>
          <code>{str("name")}</code>
          {str("preview") ? <span className="muted"> — {str("preview")}</span> : null}
        </span>
      );
    case "tool_result":
      return (
        <span>
          <code>{str("name")}</code> {p["ok"] === false ? "✗ failed" : "✓ ok"}
          {num("duration_ms") !== undefined ? <span className="muted"> · {fmtMs(num("duration_ms")!)}</span> : null}
        </span>
      );
    case "file_edit":
      return (
        <span>
          <code>{str("path")}</code>
          {num("lines") !== undefined ? <span className="muted"> · {num("lines")} lines</span> : null}
        </span>
      );
    case "approval_request":
      return <span>tool <code>{str("tool")}</code> awaiting approval</span>;
    case "approval_resolved":
      return <span>choice: <code>{str("choice")}</code></span>;
    case "error":
      return <span className="replay-err">{str("message") ?? "error"}</span>;
    case "done":
      return <span className="muted">{num("tokens") !== undefined ? `${num("tokens")} tokens` : "complete"}</span>;
    case "token":
    case "reasoning":
      return <span className="muted">{num("chars") ?? 0} chars</span>;
    default:
      return <span className="muted">{JSON.stringify(p)}</span>;
  }
}

function fmtMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
}
