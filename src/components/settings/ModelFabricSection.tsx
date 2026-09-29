import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  listEndpoints,
  saveEndpoint,
  deleteEndpoint,
  probeEndpoint,
  type EndpointCfg,
  type ProbeResult,
} from "@/lib/endpoints";
import { SettingsSection } from "./Section";

/**
 * Model Fabric — add/manage user-defined OpenAI-compatible endpoints (homelab
 * boxes on LAN/tailnet, or hosted APIs). Each becomes a `fabric-<slug>` adapter.
 * Test = an UNAUTHENTICATED probe (reachability + latency + discovered models);
 * the key is never sent on a probe.
 */
export function ModelFabricSection() {
  const [endpoints, setEndpoints] = useState<EndpointCfg[]>([]);
  const [label, setLabel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [kind, setKind] = useState<"local" | "remote">("local");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [probes, setProbes] = useState<Record<string, ProbeResult>>({});

  useEffect(() => {
    let mounted = true;
    listEndpoints()
      .then((e) => mounted && setEndpoints(e))
      .catch((e) => mounted && setErr(humanizeError(e)));
    return () => {
      mounted = false;
    };
  }, []);

  const add = async () => {
    setErr(null);
    setBusy(true);
    try {
      const list = await saveEndpoint({
        label: label.trim(),
        baseUrl: baseUrl.trim(),
        kind,
        apiKey: apiKey.trim() || undefined,
      });
      setEndpoints(list);
      setLabel("");
      setBaseUrl("");
      setApiKey("");
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const test = async (ep: EndpointCfg) => {
    setBusy(true);
    try {
      const r = await probeEndpoint(ep.base_url, ep.id);
      setProbes((p) => ({ ...p, [ep.id]: r }));
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (ep: EndpointCfg) => {
    setBusy(true);
    try {
      const list = await saveEndpoint({
        id: ep.id,
        label: ep.label,
        baseUrl: ep.base_url,
        kind: ep.kind,
        enabled: !ep.enabled,
      });
      setEndpoints(list);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (ep: EndpointCfg) => {
    setBusy(true);
    try {
      const list = await deleteEndpoint(ep.id);
      setEndpoints(list);
      setProbes((p) => {
        const { [ep.id]: _drop, ...rest } = p;
        return rest;
      });
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingsSection
      title="Model fabric"
      description={
        <>
          Add any OpenAI-compatible endpoint — a homelab vLLM/llama.cpp/LM
          Studio box on your LAN or tailnet, or a hosted API. Each becomes a
          <code> fabric-&lt;name&gt;</code> agent in the picker. “Test” probes
          it
          <strong> without sending your key</strong> (reachability + latency +
          models); the key is only used when you actually chat through it.
        </>
      }
    >
      {endpoints.length === 0 && (
        <div className="settings-muted spaced">
          No endpoints yet — add one below.
        </div>
      )}
      {endpoints.map((ep) => {
        const pr = probes[ep.id];
        return (
          <div key={ep.id} className="fabric-row">
            <div className="fabric-row-main">
              <span className="fabric-label">
                {ep.label}{" "}
                <span className={`fabric-kind ${ep.kind}`}>{ep.kind}</span>
                {!ep.enabled && (
                  <span className="fabric-kind off">disabled</span>
                )}
              </span>
              <code className="fabric-url">{ep.base_url}</code>
              {pr && (
                <span className={`fabric-probe ${pr.ok ? "ok" : "bad"}`}>
                  {pr.ok
                    ? `reachable · ${pr.latency_ms ?? "?"}ms · ${pr.models.length} models`
                    : (pr.error ?? "unreachable")}
                </span>
              )}
            </div>
            <div className="settings-row wrap">
              <button
                type="button"
                disabled={busy}
                onClick={() => void test(ep)}
              >
                Test
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => void toggle(ep)}
              >
                {ep.enabled ? "Disable" : "Enable"}
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => void remove(ep)}
              >
                Delete
              </button>
            </div>
          </div>
        );
      })}

      <div className="settings-stack tight gap-top">
        <label>
          Label
          <input
            type="text"
            value={label}
            placeholder="GPU box (vLLM)"
            onChange={(e) => setLabel(e.target.value)}
          />
        </label>
        <label>
          Base URL
          <input
            type="text"
            value={baseUrl}
            autoComplete="off"
            placeholder="http://192.168.1.50:8000/v1"
            onChange={(e) => setBaseUrl(e.target.value)}
          />
        </label>
        <label>
          API key (optional — stored in the OS keychain, never sent on a probe)
          <input
            type="password"
            value={apiKey}
            autoComplete="off"
            placeholder="sk-… (leave blank for an open endpoint)"
            onChange={(e) => setApiKey(e.target.value)}
          />
        </label>
        <div className="settings-row wrap">
          <label className="settings-check">
            <input
              type="radio"
              name="settings-fabric-kind"
              checked={kind === "local"}
              onChange={() => setKind("local")}
            />
            <span>Local (LAN/tailnet)</span>
          </label>
          <label className="settings-check">
            <input
              type="radio"
              name="settings-fabric-kind"
              checked={kind === "remote"}
              onChange={() => setKind("remote")}
            />
            <span>Remote (hosted)</span>
          </label>
          <button
            type="button"
            disabled={
              busy || label.trim().length === 0 || baseUrl.trim().length === 0
            }
            onClick={() => void add()}
          >
            Add endpoint
          </button>
        </div>
      </div>
      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}
