// Needs-attention list: pending approvals (actionable inline), failed runs,
// finished runs, quota warnings. Deliberately no "approve all".

import { useMemo, useState } from "react";
import { ApprovalCard } from "../components/ApprovalCard";
import {
  Banner,
  Chip,
  Empty,
  GroupHead,
  Row,
  Scroll,
  Skeleton,
} from "../components/ui";
import * as api from "../lib/api";
import { fmtUsd, pct100, relTime, shortId } from "../lib/format";
import { errorMessage } from "../lib/http";
import { enc, navigate, useRoute } from "../lib/nav";
import { useStore } from "../lib/store";
import type { Approval, Run, UsageReport } from "../lib/types";
import { useAsync } from "../lib/useAsync";
import { useWs } from "../lib/useWs";

const DAY = 24 * 3_600_000;

export default function InboxView() {
  const { resyncNonce, mode, toast, refreshInbox } = useStore();
  const route = useRoute();
  const focus = route.rest[0] ?? null; // #/approvals/<id>
  const legacy = mode === "legacy";
  const [busy, setBusy] = useState<Set<string>>(new Set());

  const { data, setData, error, loading, refresh } = useAsync(async () => {
    const [approvals, runs, usage] = await Promise.all([
      api.listApprovals(),
      api.hasFeature("replay") && !legacy
        ? api.listRuns(undefined, 50).catch(() => [] as Run[])
        : Promise.resolve([] as Run[]),
      api.hasFeature("usage") && !legacy
        ? api.usage().catch(() => null as UsageReport | null)
        : Promise.resolve(null as UsageReport | null),
    ]);
    return { approvals, runs, usage };
  }, [resyncNonce]);

  useWs((ev) => {
    if (
      ev.type === "approval_request" ||
      ev.type === "approval_resolved" ||
      ev.type === "done" ||
      ev.type === "error"
    ) {
      refresh();
    }
  });

  const runByRunId = useMemo(
    () => new Map((data?.runs ?? []).map((r) => [r.run_id, r])),
    [data?.runs],
  );
  const threadFor = (a: Approval) =>
    a.thread_id ?? (a.run_id ? runByRunId.get(a.run_id)?.thread_id : undefined);

  const decide = async (
    a: Approval,
    decision: "approve" | "deny",
    remember: boolean,
  ) => {
    if (busy.has(a.id)) return;
    setBusy((b) => new Set(b).add(a.id));
    try {
      await api.resolveApproval(a.id, decision, remember);
      // No optimistic removal: only drop the row once the server confirmed.
      setData((d) =>
        d ? { ...d, approvals: d.approvals.filter((x) => x.id !== a.id) } : d,
      );
      refreshInbox();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy((b) => {
        const n = new Set(b);
        n.delete(a.id);
        return n;
      });
    }
  };

  const now = Date.now();
  const failed = (data?.runs ?? []).filter(
    (r) => r.status === "error" && r.started_ms > now - 3 * DAY,
  );
  const finished = (data?.runs ?? [])
    .filter(
      (r) =>
        (r.status === "done" || r.status === "stopped") &&
        r.started_ms > now - DAY,
    )
    .slice(0, 10);
  const warnings = quotaWarnings(data?.usage ?? null);
  const approvals = data?.approvals ?? [];
  const nothing =
    approvals.length === 0 &&
    failed.length === 0 &&
    finished.length === 0 &&
    warnings.length === 0;

  return (
    <Scroll onRefresh={refresh}>
      {error && <Banner kind="error">{error}</Banner>}
      {loading ? (
        <Skeleton rows={4} />
      ) : nothing ? (
        <Empty
          icon="inbox"
          title="All clear"
          hint="Approvals, failed runs and quota warnings land here. You'll also get a badge on the tab."
          action={{ label: "Go to chats", onClick: () => navigate("/chats") }}
        />
      ) : (
        <>
          {warnings.map((w) => (
            <Banner
              key={w}
              kind="warn"
              action={{ label: "Usage", onClick: () => navigate("/runs") }}
            >
              {w}
            </Banner>
          ))}
          {approvals.length > 0 && (
            <>
              <GroupHead>Needs you · {approvals.length}</GroupHead>
              {approvals.map((a) => {
                const tid = threadFor(a);
                return (
                  <div key={a.id} id={`approval-${a.id}`}>
                    <ApprovalCard
                      approval={a}
                      compact
                      focused={focus === a.id}
                      busy={busy.has(a.id)}
                      onDecide={(d, r) => decide(a, d, r)}
                      onOpen={
                        tid
                          ? () =>
                              navigate(
                                `/threads/${enc(tid)}?approval=${enc(a.id)}`,
                              )
                          : undefined
                      }
                    />
                  </div>
                );
              })}
            </>
          )}
          {failed.length > 0 && (
            <>
              <GroupHead>Failed</GroupHead>
              <div className="list">
                {failed.map((r) => (
                  <Row
                    key={r.run_id}
                    onClick={() => navigate(`/runs/${enc(r.run_id)}`)}
                    chevron
                    leading={<Chip tone="err">error</Chip>}
                    title={`${r.agent_id ?? "run"}${r.model ? ` · ${r.model}` : ""}`}
                    sub={`run ${shortId(r.run_id)} · ${relTime(r.started_ms)}`}
                    right={
                      r.thread_id ? (
                        <button
                          className="linkbtn"
                          onClick={(e) => {
                            e.stopPropagation();
                            navigate(`/threads/${enc(r.thread_id!)}`);
                          }}
                        >
                          Chat
                        </button>
                      ) : undefined
                    }
                  />
                ))}
              </div>
            </>
          )}
          {finished.length > 0 && (
            <>
              <GroupHead>Finished</GroupHead>
              <div className="list">
                {finished.map((r) => (
                  <Row
                    key={r.run_id}
                    onClick={() =>
                      r.thread_id
                        ? navigate(`/threads/${enc(r.thread_id)}`)
                        : navigate(`/runs/${enc(r.run_id)}`)
                    }
                    chevron
                    leading={
                      <Chip tone={r.status === "done" ? "ok" : "muted"}>
                        {r.status}
                      </Chip>
                    }
                    title={`${r.agent_id ?? "run"}${r.model ? ` · ${r.model}` : ""}`}
                    sub={`${relTime(r.ended_ms ?? r.started_ms)}${r.cost_usd ? ` · ${fmtUsd(r.cost_usd)}` : ""}`}
                  />
                ))}
              </div>
            </>
          )}
        </>
      )}
    </Scroll>
  );
}

function quotaWarnings(u: UsageReport | null): string[] {
  if (!u) return [];
  const out: string[] = [];
  const check = (name: string, w: UsageReport["claude"]) => {
    if (!w) return;
    const five = pct100(w.five_hour_pct);
    const seven = pct100(w.seven_day_pct);
    if (five >= 80)
      out.push(`${name}: ${Math.round(five)}% of the 5-hour window used.`);
    if (seven >= 80)
      out.push(`${name}: ${Math.round(seven)}% of the 7-day window used.`);
  };
  check("Claude", u.claude);
  check("ChatGPT", u.chatgpt);
  const b = u.budget;
  if (
    b?.cap_usd &&
    b.spent_usd !== undefined &&
    b.spent_usd / b.cap_usd >= 0.8
  ) {
    out.push(`Budget: ${fmtUsd(b.spent_usd)} of ${fmtUsd(b.cap_usd)} spent.`);
  }
  return out;
}
