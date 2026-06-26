//! Tauri command bridge for outbound webhooks. Thin wrappers over the
//! `observability::webhooks` module so the Rust side is unit-testable
//! without a tauri runtime.

use crate::observability::webhooks::{self, TestResult, Webhook, WebhookInput};

#[tauri::command]
pub async fn list_webhooks() -> Result<Vec<Webhook>, String> {
    webhooks::load_all()
}

#[tauri::command]
pub async fn add_webhook(webhook: WebhookInput) -> Result<Webhook, String> {
    webhooks::add(webhook)
}

#[tauri::command]
pub async fn update_webhook(webhook: Webhook) -> Result<(), String> {
    webhooks::update(webhook)
}

#[tauri::command]
pub async fn delete_webhook(id: String) -> Result<(), String> {
    webhooks::delete(&id)
}

#[tauri::command]
pub async fn test_webhook(id: String) -> Result<TestResult, String> {
    webhooks::test(&id)
}

#[tauri::command]
pub async fn fire_event(event: String, payload: serde_json::Value) -> Result<u32, String> {
    Ok(webhooks::fire(&event, &payload))
}
