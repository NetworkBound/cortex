//! Persisted per-provider configuration for automatic chat-history sync.
//!
//! Stored at `~/.cortex/history_sync.json` (the same `~/.cortex` config dir the
//! rest of the app uses — see `app_state.rs`). Shape:
//!
//! ```json
//! {
//!   "claude":  { "enabled": true,  "last_sync_ts": 1718900000000, "session_source": "browser" },
//!   "chatgpt": { "enabled": false, "last_sync_ts": null,          "session_source": null }
//! }
//! ```
//!
//! No secrets ever land in this file — only flags + a timestamp + a coarse
//! provenance tag. The actual session cookie/token is fetched fresh on every
//! sync (browser auto-detect) or stored in the OS keychain (login fallback).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where the session that drives a sync came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    /// Auto-detected from an installed browser's cookie store.
    Browser,
    /// Captured via the one-time in-app login fallback webview.
    Login,
}

/// Per-provider sync settings. Defaults to disabled / never-synced / no source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Whether automatic sync is turned on for this provider.
    #[serde(default)]
    pub enabled: bool,
    /// Epoch-millis of the last successful (or attempted) sync, if any.
    #[serde(default)]
    pub last_sync_ts: Option<i64>,
    /// Where the most recent successful session came from, if known.
    #[serde(default)]
    pub session_source: Option<SessionSource>,
}

/// The whole on-disk config: a map of provider key → [`ProviderConfig`].
/// Provider keys are the canonical `"claude"` / `"chatgpt"` strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HistorySyncConfig {
    pub providers: BTreeMap<String, ProviderConfig>,
}

impl HistorySyncConfig {
    /// Borrow a provider's config (read-only), if present.
    pub fn get(&self, provider: &str) -> Option<&ProviderConfig> {
        self.providers.get(provider)
    }

    /// Get a mutable handle to a provider's config, inserting a default first.
    pub fn entry(&mut self, provider: &str) -> &mut ProviderConfig {
        self.providers.entry(provider.to_string()).or_default()
    }

    /// Providers with sync currently enabled.
    pub fn enabled_providers(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Absolute path to the config file (`~/.cortex/history_sync.json`).
/// `None` only if the home dir can't be resolved.
pub fn config_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cortex").join("history_sync.json"))
}

/// Load the config, returning a default (all-disabled) value if the file is
/// missing or unparseable — a corrupt file should never wedge the app.
pub fn load() -> HistorySyncConfig {
    let Some(path) = config_path() else {
        return HistorySyncConfig::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return HistorySyncConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Persist the config to `~/.cortex/history_sync.json`, creating `~/.cortex`
/// if needed. Returns a human-readable error string on IO failure.
pub fn save(cfg: &HistorySyncConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "could not resolve home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_serde() {
        let mut cfg = HistorySyncConfig::default();
        let c = cfg.entry("claude");
        c.enabled = true;
        c.last_sync_ts = Some(123);
        c.session_source = Some(SessionSource::Browser);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: HistorySyncConfig = serde_json::from_str(&json).unwrap();
        let got = back.get("claude").unwrap();
        assert!(got.enabled);
        assert_eq!(got.last_sync_ts, Some(123));
        assert_eq!(got.session_source, Some(SessionSource::Browser));
    }

    #[test]
    fn enabled_providers_filters() {
        let mut cfg = HistorySyncConfig::default();
        cfg.entry("claude").enabled = true;
        cfg.entry("chatgpt").enabled = false;
        assert_eq!(cfg.enabled_providers(), vec!["claude".to_string()]);
    }

    #[test]
    fn defaults_when_missing() {
        let cfg = HistorySyncConfig::default();
        assert!(cfg.get("claude").is_none());
        assert!(cfg.enabled_providers().is_empty());
    }

    #[test]
    fn session_source_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&SessionSource::Browser).unwrap(),
            "\"browser\""
        );
        assert_eq!(
            serde_json::to_string(&SessionSource::Login).unwrap(),
            "\"login\""
        );
    }
}
