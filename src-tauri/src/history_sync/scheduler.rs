//! Background scheduler for automatic chat-history sync.
//!
//! On app setup we spawn one tokio loop per **enabled** provider (read from
//! [`super::config`]). Each loop syncs immediately, then re-syncs every
//! [`super::DEFAULT_SYNC_INTERVAL_HOURS`] hours. Disabled providers are skipped;
//! toggling a provider on at runtime (via the command) spawns a fresh loop, and
//! toggling off flips the enabled flag so the loop self-exits on its next check.
//!
//! The loop re-reads config each tick so a runtime disable stops it promptly
//! without needing a restart. All syncing reuses [`super::sync_provider`], which
//! is incremental (dedup in the import pipeline). Tokens/cookies never logged.

use crate::observability::tracing_store::TracingStore;

/// Spawn background sync loops for every provider that is currently enabled in
/// the persisted config. Call once from the Tauri setup hook.
pub fn spawn_for_enabled(store: TracingStore) {
    let cfg = super::config::load();
    for provider in cfg.enabled_providers() {
        spawn_provider_loop(provider, store.clone());
    }
}

/// Spawn (or re-spawn) a single provider's sync loop. Safe to call when the
/// user enables a provider at runtime. The loop exits as soon as it observes
/// the provider disabled in config (so an enable→disable→enable can't leak
/// loops: the old one dies on its next check before the new one matters).
pub fn spawn_provider_loop(provider: String, store: TracingStore) {
    tauri::async_runtime::spawn(async move {
        // Immediate first sync on enable.
        run_once(&provider, &store).await;

        let interval = std::time::Duration::from_secs(super::DEFAULT_SYNC_INTERVAL_HOURS * 3600);
        loop {
            tokio::time::sleep(interval).await;
            // Stop if the provider was disabled while we slept.
            if !super::config::load().get(&provider).map(|c| c.enabled).unwrap_or(false) {
                tracing::info!("history_sync: provider {provider} disabled — stopping loop");
                return;
            }
            run_once(&provider, &store).await;
        }
    });
}

/// One sync pass with structured (secret-free) logging.
async fn run_once(provider: &str, store: &TracingStore) {
    match super::sync_provider(provider, store).await {
        Ok(super::SyncOutcome::Imported { result, source }) => {
            tracing::info!(
                "history_sync: {provider} synced via {:?} — {} new, {} skipped",
                source,
                result.imported,
                result.skipped
            );
        }
        Ok(super::SyncOutcome::NeedsLogin) => {
            tracing::info!("history_sync: {provider} needs login — auto-detect found no session");
        }
        Err(e) => {
            // Error strings from this module never contain the credential.
            tracing::warn!("history_sync: {provider} sync failed: {e}");
        }
    }
}
