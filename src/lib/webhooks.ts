import { invoke } from "@tauri-apps/api/core";

/**
 * Bridge for the outbound webhook egress feature (ContextForge #14).
 * Mirrors `~/.cortex/webhooks.json` via the Tauri commands defined in
 * `commands/webhooks.rs`.
 */

export interface Webhook {
  id: string;
  label: string;
  url: string;
  events: string[];
  headers: Record<string, string>;
  enabled: boolean;
}

export interface WebhookInput {
  id?: string;
  label: string;
  url: string;
  events: string[];
  headers: Record<string, string>;
  enabled: boolean;
}

export interface TestResult {
  ok: boolean;
  status: number | null;
  latency_ms: number;
  error: string | null;
}

/**
 * Curated list of events the agent runtime emits today. Users can still type
 * any custom event — this just powers autocomplete in WebhooksPanel.
 */
export const KNOWN_EVENTS: string[] = [
  "memory.snapshot.created",
  "memory.snapshot.restored",
  "task.complete",
  "task.failed",
  "approval.requested",
  "approval.granted",
  "session.start",
  "session.end",
  "agent.error",
];

export async function listWebhooks(): Promise<Webhook[]> {
  return invoke<Webhook[]>("list_webhooks");
}

export async function addWebhook(webhook: WebhookInput): Promise<Webhook> {
  return invoke<Webhook>("add_webhook", { webhook });
}

export async function updateWebhook(webhook: Webhook): Promise<void> {
  return invoke("update_webhook", { webhook });
}

export async function deleteWebhook(id: string): Promise<void> {
  return invoke("delete_webhook", { id });
}

export async function testWebhook(id: string): Promise<TestResult> {
  return invoke<TestResult>("test_webhook", { id });
}

export async function fireEvent(event: string, payload: unknown): Promise<number> {
  return invoke<number>("fire_event", { event, payload });
}
