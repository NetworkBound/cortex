//! Tauri commands for the quota-aware failover policy
//! (`~/.cortex/failover.json`, see `orchestrator::failover`).

use crate::orchestrator::failover::{load_failover_policy, write_failover_policy, FailoverPolicy};

/// Current failover policy. DEFAULT-OFF: a missing file reads as
/// `enabled: false` with an empty chain, and chat behaves exactly as today.
#[tauri::command]
pub async fn get_failover_policy() -> Result<FailoverPolicy, String> {
    Ok(load_failover_policy())
}

/// Replace the failover policy. Normalized on write (chain trimmed and
/// de-duplicated, threshold clamped to 1..=100) and echoed back. Takes effect
/// on the next message — `chat_send` re-reads the file every turn.
#[tauri::command]
pub async fn set_failover_policy(policy: FailoverPolicy) -> Result<FailoverPolicy, String> {
    write_failover_policy(&policy).map_err(|e| format!("write failover.json: {e}"))
}
