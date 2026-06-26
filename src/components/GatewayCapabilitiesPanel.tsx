import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { PanelLoading } from "./Skeleton";
import {
  formatContextWindow,
  gatewayCapabilities,
  providerState,
  type Capabilities,
  type ModelInfo,
  type ProviderInfo,
} from "@/lib/gateway-caps";
import { updateGatewayConfig } from "@/lib/cortex-bridge";
import { pushToast } from "@/lib/toast";

const REFRESH_MS = 30_000;

export function GatewayCapabilitiesPanel() {
  const [caps, setCaps] = useState<Capabilities | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeModel, setActiveModel] = useState<string | null>(null);

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const c = await gatewayCapabilities();
        if (mounted) {
          setCaps(c);
          setError(null);
        }
      } catch (e) {
        if (mounted) setError(humanizeError(e));
      }
    };
    void tick();
    const id = setInterval(tick, REFRESH_MS);
    return () => {
      mounted = false;
      clearInterval(id);
    };
  }, []);

  async function selectModel(id: string) {
    try {
      await updateGatewayConfig({ gateway_model: id });
      setActiveModel(id);
      pushToast({ title: "Active model set", body: id, kind: "success" });
    } catch (e) {
      pushToast({ title: "Failed to set model", body: humanizeError(e), kind: "error" });
    }
  }

  if (error && !caps) {
    return <div className="gateway-caps-error">{error}</div>;
  }
  if (!caps) {
    return <PanelLoading label="Loading capabilities" />;
  }

  const healthyCount = caps.providers.filter((p) => p.healthy).length;
  const providerSummary =
    caps.providers.length > 0
      ? `${healthyCount}/${caps.providers.length} providers up`
      : "no provider data";

  return (
    <div className="gateway-caps">
      <div className="gateway-caps-summary">
        <div className="gateway-caps-summary-row">
          <strong>Cortex Gateway</strong>
          <span className="muted" style={{ fontFamily: "var(--font-mono)", marginLeft: "auto" }}>
            {caps.gateway_version ?? "version unknown"}
          </span>
        </div>
        <div className="gateway-caps-summary-row muted">
          <span>{caps.models.length} models</span>
          <span>·</span>
          <span>{providerSummary}</span>
          <span style={{ marginLeft: "auto", fontFamily: "var(--font-mono)" }}>
            {caps.fetched_in_ms}ms
          </span>
        </div>
      </div>

      <div className="gateway-caps-section-title">models</div>
      {caps.models.length === 0 ? (
        <div className="muted" style={{ padding: 8, fontSize: 11.5 }}>
          No models reported by the gateway.
        </div>
      ) : (
        <div className="gateway-caps-models">
          {caps.models.map((m) => (
            <ModelCard
              key={m.id}
              model={m}
              active={activeModel === m.id}
              onPick={() => void selectModel(m.id)}
            />
          ))}
        </div>
      )}

      <div className="gateway-caps-section-title">providers</div>
      {caps.providers.length === 0 ? (
        <div className="muted" style={{ padding: 8, fontSize: 11.5 }}>
          Gateway didn't report provider health — fallback mode (using <code>/v1/models</code>).
        </div>
      ) : (
        <div className="gateway-caps-providers">
          {caps.providers.map((p) => (
            <ProviderRow key={p.name} provider={p} />
          ))}
        </div>
      )}
    </div>
  );
}

function ModelCard({
  model,
  active,
  onPick,
}: {
  model: ModelInfo;
  active: boolean;
  onPick: () => void;
}) {
  return (
    <div className={`gateway-caps-model${active ? " active" : ""}`}>
      <div className="gateway-caps-model-head">
        <code className="gateway-caps-model-id">{model.id}</code>
        {model.owner && <span className="gateway-caps-model-owner">{model.owner}</span>}
      </div>
      <div className="gateway-caps-model-meta">
        <span className="gateway-caps-ctx">ctx {formatContextWindow(model.context_window)}</span>
        {model.supports_tools && <span className="gateway-caps-badge tools">tools</span>}
        {model.supports_vision && <span className="gateway-caps-badge vision">vision</span>}
        {model.supports_reasoning && <span className="gateway-caps-badge reasoning">reasoning</span>}
      </div>
      <button className="gateway-caps-pick" onClick={onPick} title="Set as active gateway model">
        {active ? "Active" : "Use this model"}
      </button>
    </div>
  );
}

function ProviderRow({ provider }: { provider: ProviderInfo }) {
  const state = providerState(provider);
  const label = state === "ok" ? "ok" : state === "warn" ? "warn" : "down";
  const age = timeAgo(provider.last_check_ms, { empty: "—" });
  return (
    <div className={`gateway-caps-provider state-${state}`}>
      <span className={`gateway-caps-pill state-${state}`}>{label}</span>
      <span className="gateway-caps-provider-name">{provider.name}</span>
      <span className="muted" style={{ fontFamily: "var(--font-mono)", marginLeft: "auto" }}>
        {age}
      </span>
    </div>
  );
}
