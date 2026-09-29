// Runs tab: quota gauges + reliability summary, recent runs → timeline.

import { useMemo } from "react";
import { Bar, RingGauge } from "../components/Gauge";
import Icon from "../components/Icon";
import {
  Banner,
  Chip,
  Empty,
  GroupHead,
  Row,
  Scroll,
  Skeleton,
  SubHeader,
} from "../components/ui";
import * as api from "../lib/api";
import {
  fmtMs,
  fmtTokens,
  fmtUsd,
  pct,
  relTime,
  shortId,
  untilTime,
} from "../lib/format";
import { back, enc, navigate, useRoute } from "../lib/nav";
import { useStore } from "../lib/store";
import type {
  ReliabilityReport,
  ReliabilityRow,
  Run,
  TimelineEvent,
  UsageReport,
} from "../lib/types";
import { useAsync } from "../lib/useAsync";
import { useWs } from "../lib/useWs";

const STATUS_TONE: Record<string, "ok" | "warn" | "err" | "info" | "muted"> = {
  running: "info",
  done: "ok",
  error: "err",
  stopped: "muted",
};

export default function RunsView() {
  const route = useRoute();
  if (route.rest[0]) return <TimelineView runId={route.rest[0]} />;
  return <RunsList threadId={route.params.get("thread") ?? undefined} />;
}

function RunsList({ threadId }: { threadId?: string }) {
  const { mode, resyncNonce } = useStore();
  const legacy = mode === "legacy";
  const { data, error, loading, refresh } = useAsync(async () => {
    if (legacy)
      return {
        runs: [] as Run[],
        rel: null as ReliabilityReport | null,
        usage: null as UsageReport | null,
      };
    const [runs, rel, usage] = await Promise.all([
      api.hasFeature("replay")
        ? api.listRuns(threadId)
        : Promise.resolve([] as Run[]),
      api.hasFeature("reliability") && !threadId
        ? api.reliability("7d").catch(() => null)
        : Promise.resolve(null),
      api.hasFeature("usage") && !threadId
        ? api.usage().catch(() => null)
        : Promise.resolve(null),
    ]);
    return { runs, rel, usage };
  }, [threadId, resyncNonce]);
  useWs((ev) => {
    if (ev.type === "done" || ev.type === "error") refresh();
  });

  if (legacy) {
    return (
      <Empty
        icon="activity"
        title="Runs need a newer Cortex"
        hint="Update the desktop app to see run timelines, reliability and quota here."
      />
    );
  }
  const runs = data?.runs ?? [];
  return (
    <>
      {threadId && (
        <SubHeader
          title="Runs for this chat"
          onBack={() => back(`/threads/${enc(threadId)}`)}
        />
      )}
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={5} />
        ) : (
          <>
            {data?.usage && <UsageCard u={data.usage} />}
            {data?.rel && <ReliabilityCard r={data.rel} />}
            {runs.length === 0 ? (
              <Empty
                icon="activity"
                title="No runs yet"
                hint="Every reply you send becomes a run with a replayable timeline."
                action={{
                  label: "Start a chat",
                  onClick: () => navigate("/chats"),
                }}
              />
            ) : (
              <>
                <GroupHead>Recent runs</GroupHead>
                <div className="list">
                  {runs.map((r) => (
                    <Row
                      key={r.run_id}
                      chevron
                      onClick={() => navigate(`/runs/${enc(r.run_id)}`)}
                      leading={
                        <Chip tone={STATUS_TONE[r.status] ?? "muted"}>
                          {r.status}
                        </Chip>
                      }
                      title={`${r.agent_id ?? "run"}${r.model ? ` · ${r.model}` : ""}`}
                      sub={[
                        relTime(r.started_ms),
                        r.ended_ms
                          ? fmtMs(r.ended_ms - r.started_ms)
                          : r.status === "running"
                            ? "running"
                            : "",
                        r.tokens ? `${fmtTokens(r.tokens)} tok` : "",
                        r.cost_usd ? fmtUsd(r.cost_usd) : "",
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                      right={
                        <span className="mono muted small">
                          {shortId(r.run_id)}
                        </span>
                      }
                    />
                  ))}
                </div>
              </>
            )}
          </>
        )}
      </Scroll>
    </>
  );
}

function UsageCard({ u }: { u: UsageReport }) {
  const c = u.claude;
  const g = u.chatgpt;
  const b = u.budget;
  if (!c && !g && !b) return null;
  return (
    <div className="card">
      <div className="card-title">
        <Icon name="bolt" size={15} /> Quota
      </div>
      <div className="gauges">
        {c && (
          <>
            <RingGauge
              value={c.five_hour_pct}
              label="Claude 5h"
              sub={c.resets_ms ? `resets ${untilTime(c.resets_ms)}` : undefined}
            />
            <RingGauge value={c.seven_day_pct} label="Claude 7d" />
          </>
        )}
        {g && (
          <>
            <RingGauge
              value={g.five_hour_pct}
              label="ChatGPT 5h"
              sub={g.resets_ms ? `resets ${untilTime(g.resets_ms)}` : undefined}
            />
            <RingGauge value={g.seven_day_pct} label="ChatGPT 7d" />
          </>
        )}
      </div>
      {b && b.spent_usd !== undefined && (
        <Bar
          value={b.spent_usd}
          max={b.cap_usd ?? null}
          label={`Budget · ${fmtUsd(b.spent_usd)}${b.cap_usd ? ` of ${fmtUsd(b.cap_usd)}` : " spent (no cap)"}`}
        />
      )}
    </div>
  );
}

function ReliabilityCard({ r }: { r: ReliabilityReport }) {
  const t = r.totals;
  const rows = useMemo(
    () => [...(r.by_provider ?? []), ...(r.by_model ?? [])].slice(0, 6),
    [r],
  );
  if (!t && rows.length === 0) return null;
  return (
    <div className="card">
      <div className="card-title">
        <Icon name="activity" size={15} /> Reliability · 7d
      </div>
      {t && (
        <div className="stats">
          <Stat label="success" value={pct(t.success_rate)} />
          <Stat label="runs" value={String(t.runs ?? 0)} />
          <Stat label="p50" value={fmtMs(t.p50_ms) || "—"} />
          <Stat label="p95" value={fmtMs(t.p95_ms) || "—"} />
          <Stat label="cost" value={fmtUsd(t.est_usd ?? t.cost_usd) || "—"} />
        </div>
      )}
      {rows.length > 0 && (
        <div className="rel-rows">
          {rows.map((row, i) => (
            <RelRow
              key={`${row.key ?? row.agent_id ?? row.model}-${i}`}
              row={row}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function RelRow({ row }: { row: ReliabilityRow }) {
  const rate = row.success_rate ?? 0;
  const p = rate <= 1 ? rate * 100 : rate;
  return (
    <div className="rel-row">
      <span className="rel-key">{row.key ?? row.agent_id ?? row.model}</span>
      <span className="rel-bar">
        <span
          className={`rel-fill ${p >= 95 ? "ok" : p >= 80 ? "warn" : "err"}`}
          style={{ width: `${p}%` }}
        />
      </span>
      <span className="rel-val mono">{pct(row.success_rate)}</span>
      <span className="rel-lat muted">{fmtMs(row.p95_ms) || "—"}</span>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="stat">
      <div className="stat-val">{value}</div>
      <div className="stat-label">{label}</div>
    </div>
  );
}

// ── Timeline ───────────────────────────────────────────────────────────────

const KIND_LABEL: Record<string, string> = {
  prompt: "Prompt",
  route: "Routed",
  tool_call: "Tool call",
  tool_result: "Tool result",
  approval: "Approval",
  edit: "File edit",
  error: "Error",
  result: "Done",
};

function TimelineView({ runId }: { runId: string }) {
  const { data, error, loading, refresh } = useAsync(async () => {
    const [events, runs] = await Promise.all([
      api.runTimeline(runId),
      api.listRuns(undefined, 50).catch(() => [] as Run[]),
    ]);
    return { events, run: runs.find((r) => r.run_id === runId) ?? null };
  }, [runId]);
  useWs((ev) => {
    if (
      "run_id" in ev &&
      ev.run_id === runId &&
      (ev.type === "done" || ev.type === "error")
    )
      refresh();
  });
  const events = data?.events ?? [];
  const run = data?.run;
  const start = run?.started_ms ?? events[0]?.ts_ms ?? 0;
  const groups = useMemo(() => groupSteps(events), [events]);
  return (
    <>
      <SubHeader
        title={
          run
            ? `${run.agent_id ?? "run"}${run.model ? ` · ${run.model}` : ""}`
            : "Run"
        }
        sub={
          <>
            {run && (
              <Chip tone={STATUS_TONE[run.status] ?? "muted"}>
                {run.status}
              </Chip>
            )}
            {run?.ended_ms ? ` ${fmtMs(run.ended_ms - run.started_ms)}` : ""}
            {run?.tokens ? ` · ${fmtTokens(run.tokens)} tok` : ""}
            {run?.cost_usd ? ` · ${fmtUsd(run.cost_usd)}` : ""}
          </>
        }
        onBack={() => back("/runs")}
        right={
          run?.thread_id ? (
            <button
              className="iconbtn"
              aria-label="Open chat"
              onClick={() => navigate(`/threads/${enc(run.thread_id!)}`)}
            >
              <Icon name="chat" />
            </button>
          ) : undefined
        }
      />
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={5} />
        ) : events.length === 0 ? (
          <Empty
            icon="activity"
            title="No events recorded"
            hint="Tracing may be off for this run."
          />
        ) : (
          <div className="timeline">
            {groups.map((g, i) => (
              <div className={`tl-group kind-${g.kind}`} key={i}>
                <div className="tl-label">
                  {KIND_LABEL[g.kind] ?? g.kind}
                  {g.events.length > 1 && (
                    <span className="muted"> ×{g.events.length}</span>
                  )}
                  <span className="tl-at muted">
                    +{fmtMs(g.events[0].ts_ms - start)}
                  </span>
                </div>
                {g.events.map((e, j) => (
                  <details key={j} className="tl-step">
                    <summary>{e.summary}</summary>
                    {e.detail && <pre className="code">{e.detail}</pre>}
                  </details>
                ))}
              </div>
            ))}
          </div>
        )}
      </Scroll>
    </>
  );
}

/** Same grouping as the desktop Run Replay: prompt + route first, then
 *  consecutive events of one kind (e.g. a burst of tool calls) share a label. */
function groupSteps(
  events: TimelineEvent[],
): { kind: string; events: TimelineEvent[] }[] {
  const sorted = [...events].sort((a, b) => a.ts_ms - b.ts_ms);
  const head = sorted.filter((e) => e.kind === "prompt" || e.kind === "route");
  const rest = sorted.filter((e) => e.kind !== "prompt" && e.kind !== "route");
  const out: { kind: string; events: TimelineEvent[] }[] = head.map((e) => ({
    kind: e.kind,
    events: [e],
  }));
  for (const e of rest) {
    const last = out[out.length - 1];
    if (
      last &&
      last.kind === e.kind &&
      e.kind !== "error" &&
      e.kind !== "result" &&
      e.kind !== "approval"
    ) {
      last.events.push(e);
    } else {
      out.push({ kind: e.kind, events: [e] });
    }
  }
  return out;
}
