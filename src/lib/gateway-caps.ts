import { invoke } from "@tauri-apps/api/core";

export interface ModelInfo {
  id: string;
  owner: string | null;
  context_window: number | null;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_reasoning: boolean;
}

export interface ProviderInfo {
  name: string;
  healthy: boolean;
  last_check_ms: number | null;
}

export interface Capabilities {
  models: ModelInfo[];
  providers: ProviderInfo[];
  gateway_version: string | null;
  fetched_in_ms: number;
}

/** Fetch the Cortex Gateway capability surface (models + providers). */
export async function gatewayCapabilities(): Promise<Capabilities> {
  return invoke<Capabilities>("gateway_capabilities");
}

/** Compact context window label, e.g. 200_000 → "200K", 1_000_000 → "1M". */
export function formatContextWindow(n: number | null): string {
  if (n == null || n <= 0) return "—";
  // Use >= 999_950 (not 1_000_000) so values that one-decimal rounding would
  // push to "1000.0K" are promoted to "1M" instead of emitting a misleading label.
  if (n >= 999_950) {
    const v = n / 1_000_000;
    return `${v % 1 === 0 ? v.toFixed(0) : v.toFixed(1)}M`;
  }
  if (n >= 1_000) {
    const v = n / 1_000;
    return `${v % 1 === 0 ? v.toFixed(0) : v.toFixed(1)}K`;
  }
  return String(n);
}

/**
 * Classify a provider into a traffic-light state:
 *   ok   — healthy and recently checked (< 60s)
 *   warn — healthy but stale (> 60s), OR no last_check_ms at all
 *   down — `healthy: false`
 */
export function providerState(p: ProviderInfo, nowMs: number = Date.now()): "ok" | "warn" | "down" {
  if (!p.healthy) return "down";
  if (p.last_check_ms == null) return "warn";
  const ageMs = nowMs - p.last_check_ms;
  return ageMs > 60_000 ? "warn" : "ok";
}
