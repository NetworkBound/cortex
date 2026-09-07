import { useEffect, useState } from "react";
import { usageSummary, gatewayStatus, type UsageSummary } from "@/lib/usage";
import { KEEP_RECENT, shouldCompact } from "@/lib/compressor";
import { performCondense } from "@/lib/condense";
import { pushToast } from "@/lib/toast";
import { contextLimitForModel } from "@/lib/model-limits";
import { useCortexStore } from "@/state/store";

/**
 * Always-on token usage HUD pill for the StatusBar.
 *
 * Polls `usage_summary` every 10s, filters totals down to the current session,
 * and renders a compact progress pill: `<bar> <tokens>/<limit> · <pct>%`.
 * Clicking opens the Usage activity tab.
 *
 * Failure mode: if the Tauri command errors (e.g. backend warming up, no
 * tracing store yet), we render a muted `— tokens` placeholder silently rather
 * than spamming error toasts. The bar/pct just hides until data arrives.
 */

const POLL_MS = 10_000;

export function TokenHUD() {
  const sessionId = useCortexStore((s) => s.sessionId);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [errored, setErrored] = useState(false);
  // Current model id from the gateway, used to size the context window.
  // No store edit needed — gatewayStatus() already exposes the active model.
  const [model, setModel] = useState<string | null>(null);

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const s = await usageSummary();
        if (!mounted) return;
        setSummary(s);
        setErrored(false);
      } catch {
        if (!mounted) return;
        // Silent failure — see component docstring.
        setErrored(true);
      }
    };
    void tick();
    const id = setInterval(tick, POLL_MS);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  // Resolve the active model separately so a gateway hiccup here never blocks
  // the token poll above — we just fall back to the default 200k limit.
  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const g = await gatewayStatus();
        if (mounted) setModel(g.model);
      } catch {
        // Silent — contextLimitForModel(null) falls back to 200k.
      }
    };
    void tick();
    const id = setInterval(tick, POLL_MS * 3);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  // Find this session's tokens. The backend stores session_id as whatever
  // string the client passed; we just match on exact equality.
  const sessionRow = summary?.by_session.find((s) => s.session_id === sessionId);
  const tokens = sessionRow?.total_tokens ?? 0;
  const limit = contextLimitForModel(model);
  const pct = Math.min(100, (tokens / limit) * 100);

  const tone = pct < 60 ? "ok" : pct < 85 ? "warn" : "danger";
  const hasData = summary !== null && !errored;

  function openUsageTab() {
    setActivityTab("usage");
  }

  if (!hasData) {
    return (
      <button
        type="button"
        className="token-hud subtle"
        onClick={openUsageTab}
        title="Token usage (loading)"
      >
        — tokens
      </button>
    );
  }

  const compactNow = (e: React.MouseEvent) => {
    e.stopPropagation();
    const state = useCortexStore.getState();
    if (!shouldCompact(state.messages.length, KEEP_RECENT)) {
      pushToast({
        title: "Compact skipped",
        body: `Only ${state.messages.length} messages — nothing to fold.`,
        kind: "info",
      });
      return;
    }
    // Route through the single shared condenser (real LLM summary, heuristic
    // fallback) so the manual button, `/compact`, and auto-condense all behave
    // identically — previously this button used the cheap heuristic only.
    void performCondense({
      model: state.selectedModel,
      keepRecent: KEEP_RECENT,
      notify: (title, body, kind) => pushToast({ title, body, kind }),
    });
  };

  return (
    <span className="token-hud-wrap">
      <button
        type="button"
        className={`token-hud ${tone}`}
        onClick={openUsageTab}
        title={`${tokens.toLocaleString()} / ${limit.toLocaleString()} tokens (${pct.toFixed(1)}%)`}
      >
        <span className="token-hud-bar">
          <span className="fill" style={{ width: `${pct.toFixed(1)}%` }} />
        </span>
        <span className="token-hud-num">{fmtTokens(tokens)} / {fmtTokens(limit)}</span>
        <span className="token-hud-pct">· {pct.toFixed(0)}%</span>
      </button>
      {pct >= 80 && (
        <button
          type="button"
          className="token-hud-compact"
          onClick={compactNow}
          title="Context window above 80% — fold older turns into a summary"
        >
          Compact
        </button>
      )}
    </span>
  );
}

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
