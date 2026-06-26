import { useEffect, useState } from "react";
import { recentTraces, homelabHealth, type Trace, type HealthRow } from "@/lib/observability";
import { TraceDetail } from "./TraceDetail";

type Conn = "init" | "live" | "offline";

export function ObservabilityPanel() {
  const [traces, setTraces] = useState<Trace[]>([]);
  const [health, setHealth] = useState<HealthRow[]>([]);
  const [selectedTrace, setSelectedTrace] = useState<string | null>(null);
  const [conn, setConn] = useState<Conn>("init");

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const [t, h] = await Promise.all([recentTraces(8), homelabHealth()]);
        if (mounted) { setTraces(t); setHealth(h); setConn("live"); }
      } catch {
        // Gateway unreachable — keep last-known data but surface the disconnect.
        if (mounted) setConn((c) => (c === "live" ? "live" : "offline"));
      }
    };
    void tick();
    const id = setInterval(tick, 5_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  return (
    <div className="observability">
      <div className="health-strip">
        <span className={`conn-pill ${conn}`} title={
          conn === "live" ? "Connected to the gateway"
          : conn === "offline" ? "Gateway unreachable — retrying every 5s"
          : "Connecting to the gateway…"
        }>
          <span className="conn-dot" />
          {conn === "live" ? "live" : conn === "offline" ? "offline" : "connecting…"}
        </span>
        {conn === "init" && health.length === 0 && <span className="muted">services: waiting…</span>}
        {conn !== "init" && health.length === 0 && (
          <span className="muted" title="Health pollers stay idle until service targets are configured in infra.json — no network is dialed in a standalone install.">
            no health targets configured
          </span>
        )}
        {health.map((h) => (
          <span key={h.source} className={`health-dot ${h.ok ? "ok" : "off"}`} title={`${h.source} ${h.ok ? "ok" : "down"} ${h.latency_ms ?? "?"}ms`}>
            {h.source.replace(/^lxc-\d+-/, "").replace(/^host-[ab]-/, "")}
          </span>
        ))}
      </div>
      <div className="traces">
        {traces.length === 0 && (
          <div className="traces-empty">
            {conn === "offline"
              ? "Gateway offline — no live traces. Retrying…"
              : conn === "init"
                ? "Loading recent traces…"
                : "No traces yet. Run a request to see the timeline here."}
          </div>
        )}
        {traces.map((t) => {
          const start = t.spans[0]?.started_at ?? t.started_at;
          const end = t.spans.reduce((m, s) => Math.max(m, s.ended_at ?? s.started_at), start);
          const duration = Math.max(end - start, 1);
          return (
            <div
              key={t.trace_id}
              className="trace clickable"
              role="button"
              tabIndex={0}
              onClick={() => setSelectedTrace(t.trace_id)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setSelectedTrace(t.trace_id);
                }
              }}
              title="Click to open trace detail"
            >
              <div className="trace-meta">
                <code>{t.trace_id.slice(0, 8)}</code>
                <span className="muted">{new Date(start).toLocaleTimeString()}</span>
                <span className="muted">{duration}ms</span>
              </div>
              <div className="trace-bars">
                {t.spans.map((s) => {
                  const left = ((s.started_at - start) / duration) * 100;
                  const width = ((s.ended_at ?? end) - s.started_at) / duration * 100;
                  return (
                    <div key={s.id} className={`bar status-${s.status}`} style={{ left: `${left}%`, width: `${Math.max(width, 1)}%` }} title={`${s.name} ${s.agent_id ?? ""}`}>
                      <span>{s.name}{s.agent_id ? `:${s.agent_id}` : ""}</span>
                    </div>
                  );
                })}
              </div>
            </div>
          );
        })}
      </div>
      {selectedTrace && (
        <TraceDetail trace_id={selectedTrace} onClose={() => setSelectedTrace(null)} />
      )}
    </div>
  );
}
