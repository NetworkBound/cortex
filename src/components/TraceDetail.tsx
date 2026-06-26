import { useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  traceEvents,
  recentTraces,
  type Trace,
  type TraceEvent,
  type Span,
} from "@/lib/observability";
import { Chevron } from "@/lib/chevron";

interface Props {
  trace_id: string;
  onClose: () => void;
}

type StatusKey = "info" | "ok" | "running" | "error";

function statusOf(span: Span): StatusKey {
  if (span.status === "ok") return "ok";
  if (span.status === "error") return "error";
  if (span.status === "running") return "running";
  return "info";
}

function fmtTs(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour12: false }) +
    "." + String(ms % 1000).padStart(3, "0");
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function PayloadView({ payload }: { payload: unknown }) {
  const [open, setOpen] = useState(false);
  if (payload === null || payload === undefined) return null;
  const text = (() => {
    try { return JSON.stringify(payload, null, 2); }
    catch { return String(payload); }
  })();
  if (text === "{}" || text === "null") return null;
  const preview = text.length > 80 ? text.slice(0, 80) + "…" : text;
  return (
    <div className="td-payload">
      <button
        type="button"
        className="link-btn td-payload-toggle"
        onClick={() => setOpen((v) => !v)}
      >
        <Chevron open={open} size={12} />payload{!open && <span className="muted"> {preview}</span>}
      </button>
      {open && <pre className="td-payload-json">{text}</pre>}
    </div>
  );
}

export function TraceDetail({ trace_id, onClose }: Props) {
  const [trace, setTrace] = useState<Trace | null>(null);
  const [events, setEvents] = useState<TraceEvent[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedSpan, setSelectedSpan] = useState<string | null>(null);
  const eventRefs = useRef<Record<string, HTMLDivElement | null>>({});

  useEffect(() => {
    let mounted = true;
    setLoading(true);
    setErr(null);
    (async () => {
      try {
        // Trace summary isn't exposed directly; fetch the recent list and pick
        // it. Older traces fall outside a small window, so progressively widen
        // the limit until we find the trace or the list stops growing.
        const ev = await traceEvents(trace_id);
        let found: Trace | null = null;
        let prevLen = -1;
        for (const limit of [200, 1000, 5000]) {
          const list = await recentTraces(limit);
          if (!mounted) return;
          found = list.find((t) => t.trace_id === trace_id) ?? null;
          if (found || list.length === prevLen || list.length < limit) break;
          prevLen = list.length;
        }
        if (!mounted) return;
        setTrace(found);
        setEvents(ev);
      } catch (e) {
        if (mounted) setErr(humanizeError(e));
      } finally {
        if (mounted) setLoading(false);
      }
    })();
    return () => { mounted = false; };
  }, [trace_id]);

  // ESC to close
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const { start, end, duration } = useMemo(() => {
    if (!trace || trace.spans.length === 0) {
      const s = trace?.started_at ?? Date.now();
      return { start: s, end: s + 1, duration: 1 };
    }
    const s = trace.spans.reduce(
      (m, sp) => Math.min(m, sp.started_at),
      trace.spans[0].started_at,
    );
    const e = trace.spans.reduce(
      (m, sp) => Math.max(m, sp.ended_at ?? sp.started_at),
      s,
    );
    return { start: s, end: e, duration: Math.max(e - s, 1) };
  }, [trace]);

  const handleSpanClick = (spanId: string) => {
    setSelectedSpan(spanId);
    // Find the first event for this span and scroll into view.
    const span = trace?.spans.find((sp) => sp.id === spanId);
    if (!span) return;
    const target = events.findIndex((ev) => ev.span_name === span.name);
    if (target >= 0) {
      const node = eventRefs.current[`ev-${target}`];
      if (node) node.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  };

  const copyJson = async () => {
    const blob = { trace, events };
    try {
      await navigator.clipboard.writeText(JSON.stringify(blob, null, 2));
    } catch {
      // Best-effort; ignore failure
    }
  };

  const isChatTurn = useMemo(() => {
    if (!trace) return false;
    return trace.spans.some((s) => s.name === "chat.turn");
  }, [trace]);

  const replayInChat = () => {
    if (!trace) return;
    // First user message: search events for chat user content.
    let userMessage = "";
    for (const ev of events) {
      const p = ev.payload;
      if (!isPlainObject(p)) continue;
      const role = typeof p.role === "string" ? p.role : null;
      const content = typeof p.content === "string" ? p.content : null;
      if (role === "user" && content) {
        userMessage = content;
        break;
      }
      // Fall back: a "message" field on a chat.user event name
      if (ev.name.includes("user") && typeof p.message === "string") {
        userMessage = p.message;
        break;
      }
    }
    window.dispatchEvent(new CustomEvent("cortex:chat-replay", {
      detail: { trace_id: trace.trace_id, message: userMessage },
    }));
  };

  const spanEventIndex = useMemo(() => {
    const map = new Map<string, number[]>();
    events.forEach((ev, i) => {
      const arr = map.get(ev.span_name) ?? [];
      arr.push(i);
      map.set(ev.span_name, arr);
    });
    return map;
  }, [events]);

  return (
    <div
      className="modal-backdrop td-backdrop"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="trace-detail" role="dialog" aria-label="Trace detail">
        <header className="td-header">
          <div className="td-header-left">
            <div className="td-title">
              <span className="muted">trace</span>{" "}
              <code>{trace_id}</code>
            </div>
            <div className="td-stats">
              <span><span className="muted">started</span> {fmtTs(start)}</span>
              <span><span className="muted">duration</span> {duration}ms</span>
              <span><span className="muted">spans</span> {trace?.spans.length ?? 0}</span>
              <span><span className="muted">events</span> {events.length}</span>
            </div>
          </div>
          <div className="td-actions">
            <button type="button" className="link-btn" onClick={copyJson}>Copy as JSON</button>
            {isChatTurn && (
              <button type="button" className="link-btn" onClick={replayInChat}>
                Replay in chat
              </button>
            )}
            <button type="button" className="link-btn" onClick={onClose} aria-label="Close">✕</button>
          </div>
        </header>

        {err && <div className="td-error">failed to load trace events: {err}</div>}
        {loading && <div className="td-loading muted">loading…</div>}

        {!loading && trace && (
          <div className="td-body">
            <section className="td-waterfall">
              <div className="td-waterfall-axis">
                <span>0ms</span>
                <span>{Math.floor(duration / 2)}ms</span>
                <span>{duration}ms</span>
              </div>
              <div className="td-waterfall-rows">
                {trace.spans.map((s) => {
                  const left = ((s.started_at - start) / duration) * 100;
                  const w = ((s.ended_at ?? end) - s.started_at) / duration * 100;
                  const status = statusOf(s);
                  const sel = selectedSpan === s.id;
                  const spanDur = (s.ended_at ?? end) - s.started_at;
                  return (
                    <button
                      type="button"
                      key={s.id}
                      className={`td-row ${sel ? "selected" : ""}`}
                      onClick={() => handleSpanClick(s.id)}
                    >
                      <div className="td-row-label" title={s.name}>
                        <span className="td-row-name">{s.name}</span>
                        {s.agent_id && <span className="muted">:{s.agent_id}</span>}
                      </div>
                      <div className="td-row-track">
                        <div
                          className={`td-bar status-${status}`}
                          style={{ left: `${left}%`, width: `${Math.max(w, 0.5)}%` }}
                        />
                      </div>
                      <div className="td-row-dur">{spanDur}ms</div>
                    </button>
                  );
                })}
                {trace.spans.length === 0 && (
                  <div className="muted td-empty">no spans</div>
                )}
              </div>
            </section>

            <section className="td-events">
              <h3 className="td-section-title">Events</h3>
              {events.length === 0 && <div className="muted">no events</div>}
              {events.map((ev, i) => {
                const span = trace.spans.find((sp) => sp.name === ev.span_name);
                const highlight = selectedSpan && span?.id === selectedSpan;
                const status = span ? statusOf(span) : "info";
                return (
                  <div
                    key={i}
                    ref={(el) => { eventRefs.current[`ev-${i}`] = el; }}
                    className={`td-event ${highlight ? "highlight" : ""}`}
                  >
                    <div className="td-event-head">
                      <span className={`td-event-dot status-${status}`} />
                      <span className="td-event-ts">{fmtTs(ev.ts)}</span>
                      <span className="td-event-name">{ev.name}</span>
                      <span className="muted">· {ev.span_name}</span>
                      {ev.agent_id && <span className="muted">· {ev.agent_id}</span>}
                    </div>
                    <PayloadView payload={ev.payload} />
                  </div>
                );
              })}
              {selectedSpan && spanEventIndex.get(
                trace.spans.find((sp) => sp.id === selectedSpan)?.name ?? "",
              )?.length === 0 && (
                <div className="muted td-empty">no events recorded for selected span</div>
              )}
            </section>
          </div>
        )}

        {!loading && !trace && !err && (
          <div className="td-empty muted">trace not found in recent history</div>
        )}
      </div>
    </div>
  );
}
