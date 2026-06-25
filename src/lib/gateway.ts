/**
 * Gateway-configured signal.
 *
 * Some features synthesize with an LLM that today is served only by the gateway
 * gateway (deep-research planning + report synthesis, duck, …). A standalone
 * build leaves the gateway URL empty: those features can't produce output, so
 * instead of letting the user kick off a run that fails with a raw error — or,
 * worse, dialing a stale LAN endpoint — the surfaces degrade gracefully and
 * point at Settings → Connection.
 *
 * The single source of truth is the backend config's `gateway_base_url`
 * (`get_gateway_config`); a non-empty value means a gateway is configured.
 */

import { useEffect, useState } from "react";
import { getGatewayConfig } from "./cortex-bridge";

/** Window event dispatched after the gateway connection config is saved, so
 *  open surfaces re-check without a reload. Mirrors the `cortex:*` convention. */
export const GATEWAY_CONFIG_CHANGED = "cortex:gateway-config-changed";

/** True when a Cortex Gateway base URL is configured (non-empty). On any error
 *  reading config we report `false` — the safe, degrade-gracefully default. */
export async function gatewayConfigured(): Promise<boolean> {
  try {
    const cfg = await getGatewayConfig();
    return cfg.base_url.trim().length > 0;
  } catch {
    return false;
  }
}

/** Notify open surfaces that the gateway connection config changed. */
export function notifyGatewayConfigChanged(): void {
  window.dispatchEvent(new CustomEvent(GATEWAY_CONFIG_CHANGED));
}

/**
 * React hook: `null` while the first check is in flight, then a boolean.
 * Re-checks whenever the gateway config is saved (the
 * `cortex:gateway-config-changed` window event).
 */
export function useGatewayConfigured(): boolean | null {
  const [configured, setConfigured] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    const check = () => {
      void gatewayConfigured().then((v) => {
        if (alive) setConfigured(v);
      });
    };
    check();
    window.addEventListener(GATEWAY_CONFIG_CHANGED, check);
    return () => {
      alive = false;
      window.removeEventListener(GATEWAY_CONFIG_CHANGED, check);
    };
  }, []);
  return configured;
}
