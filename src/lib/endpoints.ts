import { invoke } from "@tauri-apps/api/core";

/**
 * Homelab Model Fabric bridge — user-defined OpenAI-compatible endpoints.
 * Mirrors the Rust `EndpointCfg`/`ProbeResult`. API keys are never returned by
 * these calls; they live in the OS KeyVault under `<id>/api-key`.
 */

export interface EndpointCfg {
  /** Registry id, always `fabric-<slug>`. */
  id: string;
  label: string;
  /** Base URL including `/v1`, e.g. http://192.168.1.50:8000/v1 */
  base_url: string;
  kind: "local" | "remote";
  enabled: boolean;
}

export interface ProbeResult {
  ok: boolean;
  latency_ms: number | null;
  models: string[];
  error: string | null;
}

export async function listEndpoints(): Promise<EndpointCfg[]> {
  return invoke<EndpointCfg[]>("list_endpoints");
}

/** Create/update an endpoint. `id` present = edit; omit = create from label. */
export async function saveEndpoint(args: {
  label: string;
  baseUrl: string;
  kind?: "local" | "remote";
  enabled?: boolean;
  id?: string;
  apiKey?: string;
}): Promise<EndpointCfg[]> {
  return invoke<EndpointCfg[]>("save_endpoint", {
    label: args.label,
    baseUrl: args.baseUrl,
    kind: args.kind ?? null,
    enabled: args.enabled ?? null,
    id: args.id ?? null,
    apiKey: args.apiKey ?? null,
  });
}

export async function deleteEndpoint(id: string): Promise<EndpointCfg[]> {
  return invoke<EndpointCfg[]>("delete_endpoint", { id });
}

/** Probe an endpoint (unauthenticated: reachability + latency + models). */
export async function probeEndpoint(baseUrl: string, id?: string): Promise<ProbeResult> {
  return invoke<ProbeResult>("probe_endpoint", { baseUrl, id: id ?? null });
}
