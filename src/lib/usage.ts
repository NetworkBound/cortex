import { invoke } from "@tauri-apps/api/core";

export interface UsageSummary {
  total_tokens: number;
  total_runs: number;
  session_count: number;
  by_session: SessionTokens[];
  by_provider: ProviderUsage[];
  by_model: ModelUsage[];
  upstream_pool: UpstreamProviderStatus[];
  claude_limit: ClaudeLimit | null;
}

/** Latest Claude CLI rate-limit snapshot. No precise "tokens left" is exposed
 *  by the CLI — only the rate-limit window + status. */
export interface ClaudeLimit {
  status: string | null;
  resets_at: number | null; // epoch seconds
  rate_limit_type: string | null;
  out_of_credits: boolean;
  is_using_overage: boolean;
  updated_ms: number;
}

export interface SessionTokens {
  session_id: string;
  last_active_ms: number;
  total_tokens: number;
  runs: number;
}

export interface ProviderUsage {
  agent_id: string;
  total_tokens: number;
  runs: number;
}

/** Token/run totals attributed to the effective model each run used
 *  (e.g. `claude-sonnet-4-6`), independent of which adapter routed it. */
export interface ModelUsage {
  model: string;
  agent_id: string | null;
  total_tokens: number;
  runs: number;
}

export interface UpstreamProviderStatus {
  provider: string;
  label: string | null;
  status: string;
  last_error_code: number | null;
  last_error_message: string | null;
  request_count: number | null;
  last_status_at: number | null; // epoch seconds (float)
  last_error_reset_at: number | null; // epoch seconds (float)
  auth_type: string | null;
}

export async function usageSummary(): Promise<UsageSummary> {
  return invoke<UsageSummary>("usage_summary");
}

export interface GatewayStatus {
  url: string;
  up: boolean;
  model: string | null;
  features: Record<string, unknown> | null;
  latency_ms: number | null;
}

export async function gatewayStatus(): Promise<GatewayStatus> {
  return invoke<GatewayStatus>("gateway_status");
}
