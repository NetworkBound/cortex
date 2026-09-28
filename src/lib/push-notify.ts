import { invoke } from "@tauri-apps/api/core";

/**
 * Bridge for phone push notifications (ntfy / Gotify). Mirrors
 * `src-tauri/src/commands/push_notify.rs`. The server token never crosses
 * the bridge back to the UI — `has_token` is the only signal it exists.
 */

export type PushProvider = "ntfy" | "gotify";

export const PUSH_EVENTS: { id: string; label: string; hint: string }[] = [
  {
    id: "approval_needed",
    label: "Approval needed",
    hint: "A run is waiting on Allow/Deny (deep-links to the mobile inbox).",
  },
  {
    id: "run_finished",
    label: "Run finished",
    hint: "An agent run completed successfully.",
  },
  {
    id: "run_failed",
    label: "Run failed",
    hint: "An agent run or scheduled routine errored.",
  },
  {
    id: "quota_low",
    label: "Quota low",
    hint: "A Claude/ChatGPT usage window is at 90%+.",
  },
];

export interface PushConfig {
  enabled: boolean;
  provider: PushProvider;
  server_url: string;
  topic: string;
  events: string[];
  allow_private_host: boolean;
  mobile_url: string;
}

export interface PushConfigView extends PushConfig {
  has_token: boolean;
}

export interface PushSendResult {
  ok: boolean;
  status: number | null;
  latency_ms: number;
  error: string | null;
}

export const DEFAULT_PUSH_CONFIG: PushConfig = {
  enabled: false,
  provider: "ntfy",
  server_url: "",
  topic: "",
  events: ["approval_needed", "run_failed"],
  allow_private_host: false,
  mobile_url: "",
};

export async function getPushConfig(): Promise<PushConfigView> {
  return invoke<PushConfigView>("push_get_config");
}

/** `token` non-empty stores a new token; `clearToken` removes the stored one. */
export async function setPushConfig(
  config: PushConfig,
  token?: string,
  clearToken?: boolean,
): Promise<void> {
  return invoke("push_set_config", {
    config,
    token: token && token.trim() ? token : null,
    clearToken: clearToken ?? false,
  });
}

/** Sends a synthetic notification with the *saved* config. */
export async function testPush(): Promise<PushSendResult> {
  return invoke<PushSendResult>("push_test");
}
