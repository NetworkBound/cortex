import { invoke } from "@tauri-apps/api/core";

/** Live account usage for the paid Claude + ChatGPT subscriptions. Either side
 *  may be null when the provider can't be reached (no token, host down, etc.). */
export interface AccountUsage {
  claude: ClaudeUsage | null;
  chatgpt: ChatgptUsage | null;
}

export interface ClaudeUsage {
  five_hour_pct: number;
  five_hour_resets_at: string | null; // ISO 8601
  seven_day_pct: number;
  seven_day_resets_at: string | null; // ISO 8601
  sonnet_pct: number | null;
  extra_monthly_limit: number | null;
  extra_used_credits: number | null;
  currency: string | null;
}

export interface ChatgptUsage {
  plan_type: string;
  primary_used_pct: number; // 5-hour window
  primary_reset_at: number; // unix epoch seconds
  secondary_used_pct: number; // 7-day window
  secondary_reset_at: number; // unix epoch seconds
  limit_reached: boolean;
  credits_balance: string | null;
}

export async function accountUsage(): Promise<AccountUsage> {
  return invoke<AccountUsage>("account_usage");
}
