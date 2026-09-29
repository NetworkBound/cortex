import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { useCortexStore } from "@/state/store";
import { getOutcomeRouting, setOutcomeRouting } from "@/lib/outcome-routing";
import { getSessionBudget, setSessionBudget } from "@/lib/session-budget";
import { FailoverSection } from "@/components/FailoverSettings";
import {
  LoadingHint,
  SettingsSection,
  SettingsToggle,
  UnsavedBadge,
} from "./Section";
import {
  LocalCliProvidersSection,
  OpenAiCompatProvidersSection,
  ProviderSettingsSection,
} from "./ProvidersSection";
import { ModelFabricSection } from "./ModelFabricSection";
import type { SectionDef } from "./types";

/**
 * "Outcome-aware routing" — the cost-per-success router (issue 006).
 * OPT-IN and DEFAULT-OFF. When on, Auto messages with no agent/model pick
 * prefer the provider with the best recent success-rate-per-dollar from the
 * local Reliability data; thin data means routing is unchanged. Explicit
 * picks always win. The backend flag is the source of truth.
 */
function OutcomeRoutingSection() {
  const [enabled, setEnabled] = useState<boolean | null>(null); // null = loading
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void getOutcomeRouting()
      .then((v) => {
        if (!cancelled) setEnabled(v);
      })
      .catch((e) => {
        if (!cancelled) {
          setEnabled(false);
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function toggle(next: boolean) {
    setBusy(true);
    setErr(null);
    try {
      setEnabled(await setOutcomeRouting(next));
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsSection
      title="Outcome-aware routing"
      description={
        <>
          When a message has <em>no</em> explicit agent or model pick, route it
          to the provider with the best recent success-rate-per-dollar, computed
          from your local Reliability data (Observability → Reliability). A
          provider needs at least 5 finished runs in the last 7 days to qualify;
          with thinner data routing is exactly as today. Explicit picks and
          model routes always win, and each affected message shows the rationale
          in its routing reason. Off by default.
        </>
      }
    >
      {enabled === null ? (
        <LoadingHint />
      ) : (
        <SettingsToggle
          checked={enabled}
          disabled={busy}
          onChange={(next) => void toggle(next)}
          label="Route Auto messages by cost-per-success"
          description="Prefers the cheapest provider that has been reliably succeeding for you lately. Explicit agent or model picks are never overridden."
        />
      )}
      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}

/**
 * "Session budget cap" — budget ceilings per session (issue 006 full scope).
 * OPTIONAL, off (no cap) by default. Scoped to the *current* chat session
 * (`useCortexStore`'s `sessionId`) — switching sessions/threads reloads the
 * cap for the newly active one. When spend nears the cap, outcome-aware
 * routing (above) starts preferring cheaper reliable providers; once spend
 * reaches the cap, further bare (no explicit agent/model) messages are
 * blocked by `chat_send` until the cap is raised or cleared. Explicit picks
 * and model routes are never blocked or biased. The backend is the source of
 * truth.
 */
function SessionBudgetSection() {
  const sessionId = useCortexStore((s) => s.sessionId);
  const [capInput, setCapInput] = useState(""); // "" == no cap
  const [saved, setSaved] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setErr(null);
    void getSessionBudget(sessionId)
      .then((cap) => {
        if (cancelled) return;
        setSaved(cap);
        setCapInput(cap != null ? String(cap) : "");
      })
      .catch((e) => {
        if (cancelled) return;
        setSaved(null);
        setCapInput("");
        setErr(humanizeError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  async function save() {
    const trimmed = capInput.trim();
    const cap = trimmed === "" ? null : Number(trimmed);
    if (cap != null && (!Number.isFinite(cap) || cap <= 0)) {
      setErr(
        "Enter a positive dollar amount, or leave blank to clear the cap.",
      );
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const persisted = await setSessionBudget(sessionId, cap);
      setSaved(persisted);
      setCapInput(persisted != null ? String(persisted) : "");
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function clear() {
    setBusy(true);
    setErr(null);
    try {
      await setSessionBudget(sessionId, null);
      setSaved(null);
      setCapInput("");
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  const dirty = capInput.trim() !== (saved != null ? String(saved) : "");

  return (
    <SettingsSection
      title="Session budget cap"
      description={
        <>
          Set a spend ceiling (USD) for <em>this</em> chat session, estimated
          from local token/pricing data (Observability → Reliability/Usage). As
          spend nears the cap, outcome-aware routing (above, when enabled)
          starts preferring cheaper reliable providers; once spend reaches the
          cap, further Auto messages (no explicit agent or model pick) are
          blocked until you raise or clear it. Explicit agent picks and model
          routes are never blocked or biased by a cap. No cap by default.
        </>
      }
    >
      {loading ? (
        <LoadingHint />
      ) : (
        <div className="settings-row spaced">
          <label>
            Cap (USD)
            <input
              type="number"
              min="0"
              step="0.01"
              inputMode="decimal"
              className="settings-input-sm"
              value={capInput}
              onChange={(e) => setCapInput(e.target.value)}
              placeholder="no cap"
              disabled={busy}
            />
          </label>
          <button type="button" onClick={() => void save()} disabled={busy}>
            Save
          </button>
          {saved != null && (
            <button type="button" onClick={() => void clear()} disabled={busy}>
              Clear
            </button>
          )}
          <UnsavedBadge show={dirty} />
        </div>
      )}
      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}

export const PROVIDERS_SECTIONS: SectionDef[] = [
  {
    tab: "providers",
    heading: "Providers",
    text: "providers anthropic openai api key validate model picker default model mode switch homelab claude cli login direct cloud standalone gateway bypass sign in",
    render: () => <ProviderSettingsSection />,
  },
  {
    tab: "providers",
    heading: "Local AI providers",
    text: "local cli providers claude codex openai gemini google qwen grok xai aider mistral vibe sign in login install headless detect installed authenticated terminal",
    render: () => <LocalCliProvidersSection />,
  },
  {
    tab: "providers",
    heading: "API providers",
    text: "api providers key groq together fireworks deepseek mistral xai grok perplexity openrouter dashscope qwen moonshot kimi cohere gemini llama openai compatible per-token vault",
    render: () => <OpenAiCompatProvidersSection />,
  },
  {
    tab: "providers",
    heading: "Outcome-aware routing",
    text: "outcome aware routing cost per success router reliability success rate per dollar auto route default opt-in cheapest reliable provider routing reason outcome-routing.json",
    render: () => <OutcomeRoutingSection />,
  },
  {
    tab: "providers",
    heading: "Quota-aware failover",
    text: "failover quota rate limit 429 usage limit exhausted fallback chain switch agent codex gemini ollama automatic reroute transient error failover.json",
    render: () => <FailoverSection />,
  },
  {
    tab: "providers",
    heading: "Session budget cap",
    text: "session budget cap ceiling spend limit per-session usd cost per success budget approaching exceeded block warn cheaper reliable provider session-budgets.json",
    render: () => <SessionBudgetSection />,
  },
  {
    tab: "providers",
    heading: "Model fabric",
    text: "model fabric homelab endpoint openai compatible vllm llama.cpp lmstudio lan tailnet gpu box local remote custom endpoint health probe latency discover models add edit test",
    render: () => <ModelFabricSection />,
  },
];
