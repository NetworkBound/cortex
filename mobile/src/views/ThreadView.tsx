// One conversation: streamed markdown, collapsed "Thinking…", stacked tool
// cards, a sticky approval card above the composer, error + retry, routing
// reason, usage footer, Stop, jump-to-latest and an offline outbox.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApprovalCard } from "../components/ApprovalCard";
import { Composer, type LocalCommand } from "../components/Composer";
import Icon from "../components/Icon";
import Markdown from "../components/Markdown";
import { ToolStack } from "../components/ToolCard";
import {
  ActionSheet,
  Banner,
  Chip,
  Field,
  Sheet,
  Skeleton,
  SubHeader,
} from "../components/ui";
import * as api from "../lib/api";
import { fmtTokens, fmtUsd, localId } from "../lib/format";
import { errorMessage, isApiError } from "../lib/http";
import { haptic } from "../lib/native";
import { back, enc, navigate, useRoute } from "../lib/nav";
import { useStickToBottom } from "../lib/scroll";
import { useStore } from "../lib/store";
import type {
  Approval,
  Attachment,
  Message,
  Model,
  StreamEvent,
  Thread,
  ToolCall,
} from "../lib/types";
import { baseName } from "../lib/types";
import { useWs } from "../lib/useWs";

const WATCHDOG_MS = 90_000;

export default function ThreadView({ threadId }: { threadId: string }) {
  const route = useRoute();
  const {
    mode,
    connection,
    resyncNonce,
    activeProjectRoot,
    toast,
    enqueue,
    outbox,
    refreshInbox,
  } = useStore();
  const legacy = mode === "legacy";
  const isNew = threadId === "new";

  const [messages, setMessages] = useState<Message[]>([]);
  const [thread, setThread] = useState<Thread | null>(null);
  const [loading, setLoading] = useState(!isNew);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [approval, setApproval] = useState<Approval | null>(null);
  const [approvalBusy, setApprovalBusy] = useState(false);
  const [model, setModel] = useState(route.params.get("model") ?? "");
  const [models, setModels] = useState<Model[]>([]);
  const [menu, setMenu] = useState(false);
  const [modelSheet, setModelSheet] = useState(false);
  const [rename, setRename] = useState<string | null>(null);

  // Legacy "new" sessions get their id from the first /api/chat reply.
  const sessionId = useRef<string | undefined>(isNew ? undefined : threadId);
  const projectRoot =
    route.params.get("root") ||
    thread?.project_root ||
    activeProjectRoot ||
    undefined;
  const runIdRef = useRef<string | null>(null);
  runIdRef.current = runId;
  const watchdog = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastUser = useRef<{ text: string; attachments: Attachment[] } | null>(
    null,
  );
  const {
    ref: scrollRef,
    notify,
    jump,
    atBottom,
  } = useStickToBottom<HTMLDivElement>();

  const focusApproval = route.params.get("approval");

  // ── Loading / resync ──
  const load = useCallback(async (after?: string) => {
    const id = sessionId.current;
    if (!id) return;
    try {
      const fresh = await api.getMessages(id, after ? { after } : {});
      setMessages((prev) => mergeCanonical(prev, fresh));
      setLoadError(null);
      // Derive running / pending approval state from the canonical list.
      const last = fresh[fresh.length - 1];
      if (last?.approval && !last.approval.resolved) {
        setApproval({ ...last.approval, run_id: last.run_id });
      }
      if (!after) {
        const running = fresh.some(
          (m) =>
            m.role === "assistant" &&
            m.tool_calls?.some((t) => t.status === "pending") &&
            (!m.approval || !m.approval.resolved),
        );
        if (running && last?.run_id) setRunId(last.run_id);
      }
    } catch (e) {
      setLoadError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isNew) return;
    load();
    if (!legacy) {
      api
        .listThreads()
        .then((r) => {
          const t = r.threads.find((x) => x.id === threadId);
          if (t) {
            setThread(t);
            if (t.running) setRunId((r) => r ?? "unknown");
          }
        })
        .catch(() => {});
    }
  }, [threadId, isNew, legacy, load]);

  // Reconnect / foreground: fetch only what's newer than the last server id.
  const lastServerId = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (!messages[i].id.startsWith("local-")) return messages[i].id;
    }
    return undefined;
  }, [messages]);
  const firstResync = useRef(true);
  useEffect(() => {
    if (firstResync.current) {
      firstResync.current = false;
      return;
    }
    if (sessionId.current) load(lastServerId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resyncNonce]);

  useEffect(() => {
    api
      .listModels()
      .then((r) => setModels(r.models))
      .catch(() => {});
  }, []);

  useEffect(notify, [messages, notify]);

  // Focus a specific approval card (deep link from Inbox / push).
  useEffect(() => {
    if (!focusApproval || loading) return;
    const el = document.getElementById(`approval-${focusApproval}`);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusApproval, loading, messages.length]);

  // ── Watchdog ──
  const clearWatchdog = () => {
    if (watchdog.current) clearTimeout(watchdog.current);
    watchdog.current = null;
  };
  const finishRun = useCallback(() => {
    clearWatchdog();
    setRunId(null);
  }, []);
  const armWatchdog = useCallback(() => {
    clearWatchdog();
    watchdog.current = setTimeout(() => {
      setMessages((ms) =>
        ms.map((m) =>
          m.streaming
            ? {
                ...m,
                streaming: false,
                error: "Lost the stream. Pull to refresh or try again.",
              }
            : m,
        ),
      );
      finishRun();
    }, WATCHDOG_MS);
  }, [finishRun]);
  useEffect(() => clearWatchdog, []);

  // ── Stream ──
  const patchRun = (run: string, fn: (m: Message) => Message) =>
    setMessages((ms) => {
      const i = ms.findIndex((m) => m.role === "assistant" && m.run_id === run);
      if (i < 0) {
        return [...ms, fn(placeholder(run))];
      }
      const out = [...ms];
      out[i] = fn(out[i]);
      return out;
    });

  const mine = (ev: StreamEvent) => {
    if (!("run_id" in ev) && !("thread_id" in ev)) return false;
    const tid = (ev as { thread_id?: string }).thread_id;
    const rid = (ev as { run_id?: string }).run_id;
    if (tid && sessionId.current && tid === sessionId.current) return true;
    if (rid && runIdRef.current && rid === runIdRef.current) return true;
    if (rid && messages.some((m) => m.run_id === rid)) return true;
    return false;
  };

  useWs((ev) => {
    if (ev.type === "thread_updated") {
      if (ev.thread.id === sessionId.current) setThread(ev.thread);
      return;
    }
    if (!mine(ev)) return;
    if (ev.type !== "approval_resolved") armWatchdog();
    switch (ev.type) {
      case "token":
        if (runIdRef.current === "unknown") setRunId(ev.run_id);
        patchRun(ev.run_id, (m) => ({
          ...m,
          content: m.content + ev.delta,
          streaming: true,
        }));
        break;
      case "reasoning":
        patchRun(ev.run_id, (m) => ({
          ...m,
          reasoning: (m.reasoning ?? "") + ev.delta,
          streaming: true,
        }));
        break;
      case "tool_call":
        patchRun(ev.run_id, (m) => {
          const tools = [...(m.tool_calls ?? [])];
          const i = tools.findIndex((t) => t.id === ev.tool.id);
          const tc: ToolCall = { ...ev.tool, started_ms: Date.now() };
          if (i >= 0) tools[i] = { ...tools[i], ...tc };
          else tools.push(tc);
          return { ...m, tool_calls: tools, streaming: true };
        });
        break;
      case "tool_result":
        patchRun(ev.run_id, (m) => {
          const tools = [...(m.tool_calls ?? [])];
          let i = ev.tool.id ? tools.findIndex((t) => t.id === ev.tool.id) : -1;
          if (i < 0) {
            for (let j = tools.length - 1; j >= 0; j--) {
              if (
                tools[j].status === "pending" &&
                (!ev.tool.name || tools[j].name === ev.tool.name)
              ) {
                i = j;
                break;
              }
            }
          }
          const patchTc = (t: ToolCall): ToolCall => ({
            ...t,
            ...ev.tool,
            id: t.id,
            status: ev.tool.status ?? "done",
            duration_ms:
              ev.tool.duration_ms ??
              (t.started_ms ? Date.now() - t.started_ms : undefined),
          });
          if (i >= 0) tools[i] = patchTc(tools[i]);
          else {
            tools.push(
              patchTc({
                id: localId("tc"),
                name: ev.tool.name ?? "tool",
                status: "done",
              }),
            );
          }
          return { ...m, tool_calls: tools };
        });
        break;
      case "approval_request": {
        const ap: Approval = { ...ev.approval, run_id: ev.run_id };
        patchRun(ev.run_id, (m) => ({ ...m, approval: ap }));
        setApproval(ap);
        haptic("warning");
        if (!ap.id) {
          // Legacy frames carry no id: look it up.
          api
            .listApprovals()
            .then((list) => {
              const hit = list.find((a) => a.run_id === ev.run_id);
              if (hit) setApproval({ ...ap, ...hit });
            })
            .catch(() => {});
        }
        break;
      }
      case "approval_resolved":
        setApproval((a) =>
          a && (a.id === ev.approval_id || a.run_id === ev.run_id) ? null : a,
        );
        setMessages((ms) =>
          ms.map((m) =>
            m.approval &&
            (m.approval.id === ev.approval_id || m.run_id === ev.run_id)
              ? {
                  ...m,
                  approval: { ...m.approval, resolved: true },
                  tool_calls: m.tool_calls?.map((t) =>
                    t.status === "pending"
                      ? {
                          ...t,
                          status:
                            ev.decision === "deny" ? "denied" : "approved",
                        }
                      : t,
                  ),
                }
              : m,
          ),
        );
        break;
      case "done":
        patchRun(ev.run_id, (m) => ({
          ...m,
          streaming: false,
          usage: ev.usage ?? m.usage,
          tool_calls: m.tool_calls?.map((t) =>
            t.status === "pending" ? { ...t, status: "done" } : t,
          ),
        }));
        setApproval((a) => (a && a.run_id === ev.run_id ? null : a));
        finishRun();
        haptic("success");
        // Pull the canonical rows (ids, persisted tool results).
        setTimeout(() => load(lastServerId), 400);
        break;
      case "error":
        patchRun(ev.run_id, (m) => ({
          ...m,
          streaming: false,
          error: ev.message,
        }));
        finishRun();
        break;
    }
  });

  // Queued messages that were flushed by the store: bind the run.
  useEffect(() => {
    const on = (e: Event) => {
      const d = (
        e as CustomEvent<{
          local_id: string;
          run_id: string;
          thread_id: string;
        }>
      ).detail;
      setMessages((ms) => {
        if (!ms.some((m) => m.id === d.local_id)) return ms;
        return [
          ...ms.map((m) => (m.id === d.local_id ? { ...m, queued: false } : m)),
          placeholder(d.run_id),
        ];
      });
      if (d.thread_id === sessionId.current) {
        setRunId(d.run_id);
        armWatchdog();
      }
    };
    window.addEventListener("cortex:sent", on);
    return () => window.removeEventListener("cortex:sent", on);
  }, [armWatchdog]);

  // ── Send / stop / retry ──
  const send = async (text: string, attachments: Attachment[]) => {
    lastUser.current = { text, attachments };
    const userMsg: Message = {
      id: localId("local-"),
      role: "user",
      content: text,
      ts_ms: Date.now(),
      attachments,
    };
    const offline = connection === "offline";
    setMessages((ms) => [...ms, { ...userMsg, queued: offline }]);
    jump(false);

    const body = {
      content: text,
      model: model || undefined,
      attachments: attachments.length ? attachments : undefined,
      project_root: projectRoot,
    };
    if (offline && sessionId.current) {
      enqueue({
        local_id: userMsg.id,
        thread_id: sessionId.current,
        queued_ms: Date.now(),
        ...body,
      });
      toast("You're offline — it'll send when you're back.");
      return;
    }
    setRunId("pending");
    armWatchdog();
    try {
      const r = await api.sendMessage(sessionId.current, body);
      if (!sessionId.current) {
        sessionId.current = r.thread_id;
        navigate(`/threads/${enc(r.thread_id)}`, { replace: true });
      }
      setRunId(r.run_id);
      setMessages((ms) => [...ms, placeholder(r.run_id)]);
    } catch (e) {
      if (
        (isApiError(e, "network") || isApiError(e, "timeout")) &&
        sessionId.current
      ) {
        setMessages((ms) =>
          ms.map((m) => (m.id === userMsg.id ? { ...m, queued: true } : m)),
        );
        enqueue({
          local_id: userMsg.id,
          thread_id: sessionId.current,
          queued_ms: Date.now(),
          ...body,
        });
        toast("Couldn't reach the server — queued to send.");
      } else {
        setMessages((ms) => [
          ...ms,
          {
            ...placeholder("failed-" + userMsg.id),
            streaming: false,
            error: errorMessage(e),
          },
        ]);
      }
      finishRun();
    }
  };

  const stop = async () => {
    const r = runIdRef.current;
    finishRun();
    setMessages((ms) =>
      ms.map((m) => (m.streaming ? { ...m, streaming: false } : m)),
    );
    if (!legacy && r && r !== "pending" && r !== "unknown") {
      try {
        await api.stopRun(r);
      } catch (e) {
        toast(errorMessage(e), "error");
      }
    }
    haptic("medium");
  };

  const retry = () => {
    const l = lastUser.current;
    if (!l) return;
    setMessages((ms) =>
      ms.filter((m) => !m.error || !m.id.startsWith("local-")),
    );
    send(l.text, l.attachments);
  };

  const decide = async (decision: "approve" | "deny", remember: boolean) => {
    if (!approval?.id) {
      toast("Waiting for the approval id… try again in a second.");
      return;
    }
    setApprovalBusy(true);
    try {
      await api.resolveApproval(approval.id, decision, remember);
      setApproval(null);
      refreshInbox();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setApprovalBusy(false);
    }
  };

  const onLocal = (cmd: LocalCommand) => {
    if (cmd === "clear") {
      navigate(legacy ? "/threads/new" : "/chats", { replace: true });
    } else if (cmd === "stop") {
      stop();
    } else {
      toast(
        "@diff, @status, @codebase… add context. /clear and /stop work here; other slash commands run on the desktop.",
      );
    }
  };

  const doRename = async () => {
    const t = rename?.trim();
    setRename(null);
    if (!t || !sessionId.current) return;
    try {
      const th = await api.renameThread(sessionId.current, t);
      setThread((x) => ({ ...(x ?? th), title: t }));
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const doDelete = async () => {
    if (!sessionId.current) return;
    try {
      await api.deleteThread(sessionId.current);
      back("/chats");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const running = runId !== null;
  const queuedHere = outbox.filter(
    (o) => o.thread_id === sessionId.current,
  ).length;
  const title = thread?.title || (isNew ? "New chat" : "Chat");
  const modelLabel =
    models.find((m) => m.id === model)?.label || model || "Auto";

  return (
    <div className="thread">
      <SubHeader
        title={title}
        sub={
          <>
            {projectRoot ? baseName(projectRoot) : "No project"}
            {thread?.agent_id ? ` · ${thread.agent_id}` : ""}
            {thread?.model ? ` · ${thread.model}` : ""}
          </>
        }
        onBack={() => back("/chats")}
        right={
          !legacy &&
          !isNew && (
            <button
              className="iconbtn"
              aria-label="Chat options"
              onClick={() => setMenu(true)}
            >
              <Icon name="more" />
            </button>
          )
        }
      />

      <div className="scroll thread-scroll" ref={scrollRef}>
        {loadError && (
          <Banner
            kind="error"
            action={{ label: "Retry", onClick: () => load() }}
          >
            {loadError}
          </Banner>
        )}
        {loading ? (
          <Skeleton rows={4} />
        ) : messages.length === 0 ? (
          <div className="thread-empty">
            <Icon name="spark" size={30} />
            <p>
              Ask anything. Use <code>@diff</code>, <code>@status</code> or{" "}
              <code>@codebase</code> to pull in project context.
            </p>
          </div>
        ) : (
          <div className="msg-list">
            {messages.map((m) => (
              <MessageRow
                key={m.id}
                m={m}
                onRetry={retry}
                focusApproval={focusApproval}
              />
            ))}
          </div>
        )}
      </div>

      {!atBottom && (
        <button
          className="jump"
          onClick={() => jump()}
          aria-label="Jump to latest"
        >
          <Icon name="arrowDown" size={18} />
        </button>
      )}

      {approval && (
        <div className="approval-dock">
          <ApprovalCard
            approval={approval}
            busy={approvalBusy}
            onDecide={decide}
          />
        </div>
      )}

      <div className="composer-meta">
        <button className="meta-chip" onClick={() => setModelSheet(true)}>
          <Icon name="bolt" size={13} /> {modelLabel}
        </button>
        {queuedHere > 0 && <Chip tone="warn">{queuedHere} queued</Chip>}
        {connection !== "connected" && (
          <Chip tone={connection === "offline" ? "err" : "warn"}>
            {connection === "offline" ? "offline" : "reconnecting"}
          </Chip>
        )}
      </div>
      <Composer
        running={running}
        allowAttachments={!legacy}
        onSend={send}
        onStop={stop}
        onLocal={onLocal}
        notify={(t) => toast(t)}
        draftKey={isNew ? undefined : threadId}
        placeholder={running ? "Reply is streaming…" : "Message Cortex…"}
      />

      <ActionSheet
        open={menu}
        onClose={() => setMenu(false)}
        actions={[
          { label: "Rename", icon: "edit", onClick: () => setRename(title) },
          {
            label: "View runs",
            icon: "activity",
            onClick: () => navigate(`/runs?thread=${enc(threadId)}`),
          },
          {
            label: "Delete chat",
            icon: "trash",
            destructive: true,
            onClick: doDelete,
          },
        ]}
      />
      <Sheet
        open={rename !== null}
        onClose={() => setRename(null)}
        title="Rename chat"
      >
        <Field label="Title">
          <input
            value={rename ?? ""}
            autoFocus
            onChange={(e) => setRename(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && doRename()}
          />
        </Field>
        <button className="btn primary block" onClick={doRename}>
          Save
        </button>
      </Sheet>
      <ActionSheet
        open={modelSheet}
        onClose={() => setModelSheet(false)}
        title="Model for new messages"
        actions={[
          {
            label: "Auto (router decides)",
            icon: model ? undefined : "check",
            onClick: () => setModel(""),
          },
          ...models.map((m) => ({
            label: `${m.label}${m.provider ? ` · ${m.provider}` : ""}${m.local ? " · local" : ""}`,
            icon: m.id === model ? "check" : undefined,
            onClick: () => setModel(m.id),
          })),
        ]}
      />
    </div>
  );
}

function placeholder(run_id: string): Message {
  return {
    id: `local-${run_id}`,
    role: "assistant",
    content: "",
    ts_ms: Date.now(),
    run_id,
    streaming: true,
    tool_calls: [],
  };
}

/** Merge a canonical page from the server into the current list: rows are
 *  matched by id, then a server assistant row replaces our local placeholder
 *  for the same run. Unknown local rows (queued / in-flight) are kept. */
function mergeCanonical(prev: Message[], fresh: Message[]): Message[] {
  if (prev.length === 0) return fresh;
  const byId = new Map(prev.map((m) => [m.id, m]));
  const seenRuns = new Set<string>();
  const out: Message[] = [];
  const freshIds = new Set(fresh.map((m) => m.id));
  // Keep local rows that the server doesn't know about yet, in place.
  for (const m of prev) {
    if (freshIds.has(m.id)) continue;
    if (!m.id.startsWith("local-")) {
      // Older server rows not in this (after=) page: keep.
      out.push(m);
    }
  }
  for (const f of fresh) {
    const local = byId.get(f.id);
    if (f.role === "assistant" && f.run_id) seenRuns.add(f.run_id);
    out.push(
      local
        ? {
            ...local,
            ...f,
            streaming: local.streaming && !f.content ? local.streaming : false,
          }
        : f,
    );
  }
  // Re-add local rows not superseded by a server row for the same run.
  for (const m of prev) {
    if (!m.id.startsWith("local-") || freshIds.has(m.id)) continue;
    if (m.role === "assistant" && m.run_id && seenRuns.has(m.run_id)) continue;
    if (
      m.role === "user" &&
      !m.queued &&
      fresh.some((f) => f.role === "user" && f.content === m.content)
    )
      continue;
    out.push(m);
  }
  out.sort((a, b) => (a.ts_ms ?? 0) - (b.ts_ms ?? 0));
  return out;
}

function MessageRow({
  m,
  onRetry,
  focusApproval,
}: {
  m: Message;
  onRetry: () => void;
  focusApproval: string | null;
}) {
  if (m.role === "system") return null;
  if (m.role === "user") {
    return (
      <div className={`msg user ${m.queued ? "queued" : ""}`}>
        {m.attachments && m.attachments.length > 0 && (
          <div className="msg-attachments">
            {m.attachments.map((a, i) => (
              <img
                key={i}
                src={`data:${a.mime};base64,${a.data_base64}`}
                alt={a.name}
              />
            ))}
          </div>
        )}
        {m.content}
        {m.queued && <span className="queued-tag">queued</span>}
      </div>
    );
  }
  const empty = !m.content && !m.reasoning && !m.tool_calls?.length && !m.error;
  return (
    <div className={`msg assistant ${m.streaming ? "streaming" : ""}`}>
      {m.routing_reason && (
        <div className="routing">
          <Icon name="git" size={12} /> {m.routing_reason}
        </div>
      )}
      {m.reasoning && (
        <details className="thinking">
          <summary>
            <Icon name="spark" size={13} />
            {m.streaming && !m.content ? "Thinking…" : "Thought process"}
          </summary>
          <div className="thinking-body">{m.reasoning}</div>
        </details>
      )}
      {m.tool_calls && m.tool_calls.length > 0 && (
        <ToolStack tools={m.tool_calls} />
      )}
      {m.content ? (
        <Markdown>{m.content}</Markdown>
      ) : (
        empty &&
        m.streaming && (
          <span className="typing" aria-label="Waiting for reply" />
        )
      )}
      {m.approval && (
        <div
          id={`approval-${m.approval.id}`}
          className={`approval-inline ${m.approval.resolved ? "resolved" : ""} ${
            focusApproval === m.approval.id ? "focused" : ""
          }`}
        >
          <Icon name="warning" size={13} />
          {m.approval.resolved
            ? "Approval resolved"
            : "Waiting for your approval"}
          {m.approval.tool ? ` · ${m.approval.tool}` : ""}
        </div>
      )}
      {m.error && (
        <div className="error-card" role="alert">
          <div className="error-text">{m.error}</div>
          <button className="btn small" onClick={onRetry}>
            <Icon name="refresh" size={14} /> Retry
          </button>
        </div>
      )}
      {!m.streaming && m.usage && (
        <div className="usage-foot">
          {m.usage.input_tokens !== undefined &&
          m.usage.output_tokens !== undefined
            ? `${fmtTokens(m.usage.input_tokens)} in · ${fmtTokens(m.usage.output_tokens)} out`
            : m.usage.total_tokens !== undefined
              ? `${fmtTokens(m.usage.total_tokens)} tok`
              : ""}
          {m.usage.cost_usd !== undefined && m.usage.cost_usd !== null
            ? ` · ${fmtUsd(m.usage.cost_usd)}`
            : ""}
        </div>
      )}
    </div>
  );
}
