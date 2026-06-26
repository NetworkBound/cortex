import { useEffect, useState } from "react";
import { TriangleAlert } from "lucide-react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import {
  gatewayStatus,
  usageSummary,
  type ClaudeLimit,
  type GatewayStatus,
  type UpstreamProviderStatus,
  type UsageSummary,
} from "@/lib/usage";
import {
  accountUsage,
  type AccountUsage,
  type ChatgptUsage,
  type ClaudeUsage,
} from "@/lib/account-usage";

export function UsageView() {
  const [data, setData] = useState<UsageSummary | null>(null);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [acct, setAcct] = useState<AccountUsage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const [s, g] = await Promise.all([usageSummary(), gatewayStatus()]);
        if (mounted) {
          setData(s);
          setGateway(g);
          setError(null);
        }
      } catch (e) {
        // Keep any previously-loaded data on screen across a transient poll
        // failure; only surface the error when we have nothing to show.
        if (mounted) setError(humanizeError(e));
      } finally {
        if (mounted) setLoading(false);
      }
    };
    void tick();
    const id = setInterval(tick, 8_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  // Live account usage polls the external provider endpoints on a gentler 60s
  // cadence than the local usage poll above — these hit api.anthropic.com and
  // ssh to CT154, so we don't hammer them.
  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const a = await accountUsage();
        if (mounted) setAcct(a);
      } catch { /* */ }
    };
    void tick();
    const id = setInterval(tick, 60_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  if (!data && loading) return <PanelLoading label="Loading usage" />;
  if (!data) {
    return (
      <div className="usage-view">
        <div className="usage-error">
          {error ?? "No usage data available yet."}
        </div>
      </div>
    );
  }

  return (
    <div className="usage-view">
      <LiveAccountUsage acct={acct} />
      {gateway && (
        <div className={`usage-gateway ${gateway.up ? "up" : "down"}`}>
          <div className="usage-gateway-head">
            <span className={`dot ${gateway.up ? "ok" : "off"}`} />
            <strong>Cortex Gateway</strong>
            <span className="muted" style={{ fontFamily: "var(--font-mono)", marginLeft: "auto" }}>
              {gateway.latency_ms != null ? `${gateway.latency_ms}ms` : "—"}
            </span>
          </div>
          <div className="muted usage-gateway-url">{gateway.url}</div>
          {gateway.model && (
            <div className="muted usage-gateway-model">
              model: <code>{gateway.model}</code>
            </div>
          )}
        </div>
      )}
      <ProviderLimits claude={data.claude_limit} pool={data.upstream_pool} />

      <div className="usage-stats">
        <div className="usage-stat">
          <div className="usage-stat-label">tokens used</div>
          <div className="usage-stat-value">{fmtNum(data.total_tokens)}</div>
        </div>
        <div className="usage-stat">
          <div className="usage-stat-label">runs</div>
          <div className="usage-stat-value">{fmtNum(data.total_runs)}</div>
        </div>
        <div className="usage-stat">
          <div className="usage-stat-label">sessions</div>
          <div className="usage-stat-value">{fmtNum(data.session_count)}</div>
        </div>
      </div>

      {data.by_provider.length > 0 && (
        <>
          <div className="usage-section-title">by provider</div>
          <div className="usage-bars">
            {data.by_provider.map((p) => {
              const max = Math.max(...data.by_provider.map((x) => x.total_tokens), 1);
              const width = (p.total_tokens / max) * 100;
              return (
                <div key={p.agent_id} className="usage-bar">
                  <div className="usage-bar-head">
                    <span>{p.agent_id}</span>
                    <span className="muted">{fmtNum(p.total_tokens)} · {p.runs} runs</span>
                  </div>
                  <div className="usage-bar-track">
                    <div className="usage-bar-fill" style={{ width: `${width}%` }} />
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}

      {data.by_model.length > 0 && (
        <>
          <div className="usage-section-title">by model</div>
          <div className="usage-bars">
            {data.by_model.map((m) => {
              const max = Math.max(...data.by_model.map((x) => x.total_tokens), 1);
              const width = (m.total_tokens / max) * 100;
              return (
                <div key={m.model} className="usage-bar">
                  <div className="usage-bar-head">
                    <span>
                      {m.model}
                      {m.agent_id && <span className="muted"> · {m.agent_id}</span>}
                    </span>
                    <span className="muted">{fmtNum(m.total_tokens)} · {m.runs} runs</span>
                  </div>
                  <div className="usage-bar-track">
                    <div className="usage-bar-fill" style={{ width: `${width}%` }} />
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}

      {data.by_session.length > 0 && (
        <>
          <div className="usage-section-title">recent sessions</div>
          <div className="usage-sessions">
            {data.by_session.slice(0, 12).map((s) => (
              <div key={s.session_id} className="usage-session-row">
                <span className="usage-session-id">{s.session_id.slice(-12)}</span>
                <span className="muted" style={{ fontFamily: "var(--font-mono)" }}>
                  {timeAgo(s.last_active_ms)}
                </span>
                <span className="usage-tokens">{fmtNum(s.total_tokens)}t</span>
                <span className="muted" style={{ fontFamily: "var(--font-mono)" }}>{s.runs}r</span>
              </div>
            ))}
          </div>
        </>
      )}

      {data.upstream_pool.length > 0 && (
        <>
          <div className="usage-section-title">upstream credential pool (live)</div>
          <div className="usage-pool">
            {data.upstream_pool.map((p, i) => {
              const isOk = !p.status || p.status === "ready";
              const cls = isOk ? "ok" : p.status === "exhausted" ? "exhausted" : "error";
              return (
                <div key={`${p.provider}-${i}`} className={`usage-pool-row ${cls}`}>
                  <div className="usage-pool-head">
                    <strong>{p.provider}</strong>
                    {p.label && <span className="muted usage-pool-label">{p.label}</span>}
                    <span className={`usage-pool-status ${cls}`}>{isOk ? "ready" : p.status}</span>
                  </div>
                  {p.last_error_code != null && (
                    <div className="muted usage-pool-error">
                      {p.last_error_code} · {p.last_error_message ?? ""}
                    </div>
                  )}
                  {p.request_count != null && (
                    <div className="muted usage-pool-requests">
                      {p.request_count} requests
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </>
      )}
      {data.upstream_pool.length === 0 && (
        <>
          <div className="usage-section-title">upstream pool</div>
          <div className="muted usage-pool-empty">
            No upstream pool data — SSH to the gateway host may have failed (check key auth or that the LXC is reachable).
          </div>
        </>
      )}
    </div>
  );
}

/**
 * Prominent "Live Account Usage" section: real, climbing "used %" pulled live
 * from the Claude (Max) and ChatGPT (Plus) subscription endpoints. Each side is
 * independent — a null provider renders a muted "not connected" placeholder.
 */
function LiveAccountUsage({ acct }: { acct: AccountUsage | null }) {
  const claude = acct?.claude ?? null;
  const chatgpt = acct?.chatgpt ?? null;
  return (
    <div className="acct-usage">
      <div className="usage-section-title">live account usage</div>
      <div className="acct-usage-cards">
        <ClaudeAccountCard claude={claude} />
        <ChatgptAccountCard chatgpt={chatgpt} />
      </div>
    </div>
  );
}

function ClaudeAccountCard({ claude }: { claude: ClaudeUsage | null }) {
  if (!claude) {
    return (
      <div className="acct-card">
        <div className="acct-card-head">
          <strong>Claude</strong>
          <span className="acct-card-plan">Max</span>
        </div>
        <div className="muted acct-card-empty">Claude · not connected</div>
      </div>
    );
  }
  const fiveReset = fmtResetInISO(claude.five_hour_resets_at);
  const sevenReset = fmtResetInISO(claude.seven_day_resets_at);
  return (
    <div className="acct-card">
      <div className="acct-card-head">
        <strong>Claude</strong>
        <span className="acct-card-plan">Max</span>
      </div>
      <AcctBar
        label="5h"
        pct={claude.five_hour_pct}
        reset={fiveReset}
      />
      <AcctBar
        label="7d"
        pct={claude.seven_day_pct}
        reset={sevenReset}
        thin
      />
      {claude.sonnet_pct != null && (
        <AcctBar label="7d sonnet" pct={claude.sonnet_pct} thin />
      )}
      {claude.extra_monthly_limit != null && (
        <div className="acct-card-extra muted">
          Extra credits: {fmtMoney(claude.extra_used_credits, claude.currency)} /{" "}
          {fmtMoney(claude.extra_monthly_limit, claude.currency)}
        </div>
      )}
    </div>
  );
}

function ChatgptAccountCard({ chatgpt }: { chatgpt: ChatgptUsage | null }) {
  if (!chatgpt) {
    return (
      <div className="acct-card">
        <div className="acct-card-head">
          <strong>ChatGPT</strong>
          <span className="acct-card-plan">Plus</span>
        </div>
        <div className="muted acct-card-empty">ChatGPT · not connected</div>
      </div>
    );
  }
  const plan = chatgpt.plan_type
    ? chatgpt.plan_type.charAt(0).toUpperCase() + chatgpt.plan_type.slice(1)
    : "Plus";
  return (
    <div className="acct-card">
      <div className="acct-card-head">
        <strong>ChatGPT</strong>
        <span className="acct-card-plan">{plan}</span>
        {chatgpt.limit_reached && (
          <span className="acct-card-limit">limit reached</span>
        )}
      </div>
      <AcctBar
        label="5h"
        pct={chatgpt.primary_used_pct}
        reset={fmtResetInEpoch(chatgpt.primary_reset_at)}
      />
      <AcctBar
        label="7d"
        pct={chatgpt.secondary_used_pct}
        reset={fmtResetInEpoch(chatgpt.secondary_reset_at)}
        thin
      />
      {chatgpt.credits_balance != null &&
        chatgpt.credits_balance !== "0" && (
          <div className="acct-card-extra muted">
            Credits balance: {chatgpt.credits_balance}
          </div>
        )}
    </div>
  );
}

/** A single labelled usage bar, colored by utilization band. */
function AcctBar({
  label,
  pct,
  reset,
  thin,
}: {
  label: string;
  pct: number;
  reset?: string;
  thin?: boolean;
}) {
  const clamped = Math.max(0, Math.min(100, pct));
  const band = clamped > 90 ? "danger" : clamped >= 70 ? "warning" : "success";
  return (
    <div className="acct-bar">
      <div className="acct-bar-head">
        <span className="acct-bar-label">
          {label} · {Math.round(pct)}%
          {reset && <span className="muted"> · resets in {reset}</span>}
        </span>
      </div>
      <div className={`acct-bar-track ${thin ? "thin" : ""}`}>
        <div
          className={`acct-bar-fill ${band}`}
          style={{ width: `${clamped}%` }}
        />
      </div>
    </div>
  );
}

/** Format a money amount with currency, e.g. (0, "USD") → "$0.00". */
function fmtMoney(amount: number | null, currency: string | null): string {
  const n = amount ?? 0;
  const sym = currency === "USD" || !currency ? "$" : `${currency} `;
  return `${sym}${n.toFixed(2)}`;
}

/** "resets in Xh Ym" from a unix-epoch-seconds reset time. "" for 0/null. */
function fmtResetInEpoch(sec: number | null): string {
  if (!sec) return "";
  return fmtCountdown(Math.floor(sec - Date.now() / 1000));
}

/** "resets in Xh Ym" from an ISO 8601 timestamp. "" for null/unparseable. */
function fmtResetInISO(s: string | null): string {
  if (!s) return "";
  const ms = Date.parse(s);
  if (Number.isNaN(ms)) return "";
  return fmtCountdown(Math.floor((ms - Date.now()) / 1000));
}

/** Shared seconds → "Xh Ym" / "Ym" / "Xs" countdown formatter. */
function fmtCountdown(secs: number): string {
  if (secs <= 0) return "soon";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${secs}s`;
}

/**
 * At-a-glance "Provider Limits" overview. Real rate-limit/status only — the
 * upstreams don't expose a precise "tokens left" number, so we show window +
 * status + reset countdowns instead.
 */
function ProviderLimits({
  claude,
  pool,
}: {
  claude: ClaudeLimit | null;
  pool: UpstreamProviderStatus[];
}) {
  return (
    <div className="provider-limits">
      <div className="usage-section-title">provider limits</div>
      <div className="provider-limits-rows">
        <ClaudeLimitRow claude={claude} />
        {pool.map((p, i) => (
          <GatewayLimitRow key={`${p.provider}-${i}`} p={p} />
        ))}
        <div className="provider-limit-row">
          <span className="provider-limit-name">Ollama</span>
          <span className="provider-limit-badge ok">no limits</span>
          <span className="muted provider-limit-meta">local models</span>
        </div>
      </div>
    </div>
  );
}

function ClaudeLimitRow({ claude }: { claude: ClaudeLimit | null }) {
  if (!claude) {
    return (
      <div className="provider-limit-row">
        <span className="provider-limit-name">Claude</span>
        <span className="muted provider-limit-meta">no recent activity</span>
      </div>
    );
  }
  const limited = claude.status != null && claude.status !== "allowed";
  const badgeCls = limited ? "warn" : "ok";
  const reset = fmtResetIn(claude.resets_at);
  return (
    <div className="provider-limit-row">
      <span className="provider-limit-name">Claude</span>
      <span className={`provider-limit-badge ${badgeCls}`}>
        {claude.status ?? "—"}
      </span>
      <span className="muted provider-limit-meta">
        {humanizeWindow(claude.rate_limit_type)}
        {reset && <> · resets in {reset}</>}
      </span>
      {claude.out_of_credits && (
        <span className="provider-limit-warn">
          <TriangleAlert size={12} strokeWidth={1.75} aria-hidden="true" /> out of credits
        </span>
      )}
    </div>
  );
}

function GatewayLimitRow({ p }: { p: UpstreamProviderStatus }) {
  const status = p.status || "ready";
  const badgeCls =
    status === "ready" ? "ok" : status === "exhausted" ? "warn" : "err";
  const name = p.label || p.provider;
  const reset = fmtResetIn(p.last_error_reset_at);
  const isErr = badgeCls === "err";
  return (
    <div className="provider-limit-row">
      <span className="provider-limit-name">{name}</span>
      <span className={`provider-limit-badge ${badgeCls}`}>{status}</span>
      <span className="muted provider-limit-meta">
        {p.request_count != null && <>{fmtNum(p.request_count)} reqs</>}
        {reset && <> · resets in {reset}</>}
      </span>
      {isErr && p.last_error_message && (
        <span className="provider-limit-err" title={p.last_error_message}>
          {p.last_error_message.slice(0, 60)}
        </span>
      )}
    </div>
  );
}

/** Humanize a rate-limit window key, e.g. "five_hour" → "5-hour window". */
function humanizeWindow(kind: string | null): string {
  if (!kind) return "";
  const map: Record<string, string> = {
    five_hour: "5-hour window",
    seven_day: "7-day window",
    one_hour: "1-hour window",
    one_minute: "1-minute window",
  };
  if (map[kind]) return map[kind];
  // Generic fallback: "<n>_<unit>" → "<n>-<unit> window".
  return `${kind.replace(/_/g, "-")} window`;
}

/**
 * Format an epoch (seconds; accepts float) into a "Xh Ym" countdown from now.
 * Recomputes each render (the dashboard re-polls every ~10s). Returns "" for a
 * null/zero input and "resets soon" when the reset is already in the past.
 */
function fmtResetIn(epochSeconds: number | null): string {
  if (!epochSeconds) return "";
  const secs = Math.floor(epochSeconds - Date.now() / 1000);
  if (secs <= 0) return "resets soon";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${secs}s`;
}

function fmtNum(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

