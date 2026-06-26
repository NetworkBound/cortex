//! Deployment-infrastructure endpoints — `~/.cortex/infra.json`.
//!
//! Shipped binaries must contain NO hardcoded LAN / VPN / homelab addresses.
//! Every infrastructure endpoint the app dials resolves through this module
//! with the order:
//!
//!   1. environment variable (per-endpoint, listed below), then
//!   2. `~/.cortex/infra.json`, then
//!   3. `None` — the dependent feature humanizes to "not configured" and
//!      performs NO network I/O (no LAN dialing, no error spam).
//!
//! `~/.cortex/infra.json` shape (every key optional):
//!
//! ```json
//! {
//!   "gateway_base_url":  "http://gateway-host:8642",
//!   "ollama_base_url":   "http://ollama-host:11434",
//!   "update_gitea_host": "http://git-host:3000",
//!   "update_pubkey":     "base64-of-raw-32-byte-ed25519-public-key",
//!   "usage_ssh_host":    "root@hypervisor-host",
//!   "health_targets": [
//!     { "source": "my-gateway", "url": "http://gateway-host:8642/health" }
//!   ]
//! }
//! ```
//!
//! | key                 | env override              | consumer                          |
//! |---------------------|---------------------------|-----------------------------------|
//! | `gateway_base_url`  | `CORTEX_GATEWAY_BASE_URL` | gateway adapter / app config      |
//! | `ollama_base_url`   | `OLLAMA_BASE_URL`         | local Ollama adapter              |
//! | `update_gitea_host` | `CORTEX_UPDATE_GITEA_HOST`| AppImage self-update (selfupdate) |
//! | `update_pubkey`     | `CORTEX_UPDATE_PUBKEY`    | self-update signature verification |
//! | `usage_ssh_host`    | `CORTEX_USAGE_SSH_HOST`   | usage/credential-pool SSH polling |
//! | `health_targets`    | —                         | observability health pollers      |

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One health-poller probe target (see `observability::homelab`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HealthTargetEntry {
    /// Stable label the sample is recorded under (e.g. `"my-gateway"`).
    pub source: String,
    /// Full URL probed with a GET (e.g. `"http://host:8642/health"`).
    pub url: String,
}

/// Parsed `~/.cortex/infra.json`. Every field is optional; absent keys mean
/// "not configured" and the dependent feature no-ops gracefully.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InfraConfig {
    /// Cortex Gateway base URL. `serde(alias)` keeps reading the legacy
    /// `hermes_base_url` key so an existing `~/.cortex/infra.json` written
    /// before the rebrand keeps working with zero manual migration.
    #[serde(default, alias = "hermes_base_url")]
    pub gateway_base_url: Option<String>,
    #[serde(default)]
    pub ollama_base_url: Option<String>,
    /// Gitea base (scheme + host + port) hosting the `cortex` release repo
    /// the AppImage self-updater pulls from.
    #[serde(default)]
    pub update_gitea_host: Option<String>,
    /// Override for the baked-in ed25519 update-signing public key (base64 of
    /// the raw 32-byte key). Lets a key rotation take effect without a rebuild.
    /// Absent → the compiled-in `selfupdate::DEFAULT_UPDATE_PUBKEY` is used.
    #[serde(default)]
    pub update_pubkey: Option<String>,
    /// `user@host` the usage pollers SSH to for upstream credential-pool /
    /// ChatGPT usage JSON. Unset → usage panels show only local data.
    #[serde(default)]
    pub usage_ssh_host: Option<String>,
    /// Endpoints the observability health pollers probe every 30s. Empty →
    /// the poller loop idles without dialing anything.
    #[serde(default)]
    pub health_targets: Vec<HealthTargetEntry>,
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cortex/infra.json"))
}

/// Read + parse `~/.cortex/infra.json`. Missing or malformed files quietly
/// degrade to the all-`None` default — never an error, never a panic.
pub fn load_file() -> InfraConfig {
    let Some(path) = config_path() else {
        return InfraConfig::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return InfraConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Resolution core: prefer the env value, then the file value; trim both and
/// treat empty/whitespace as unset. Pure, so the precedence is unit-testable.
fn first_configured(env_val: Option<String>, file_val: Option<&str>) -> Option<String> {
    env_val
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| file_val.map(str::trim).filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// Same as [`first_configured`] but normalized as a base URL (no trailing `/`).
fn first_configured_url(env_val: Option<String>, file_val: Option<&str>) -> Option<String> {
    first_configured(env_val, file_val).map(|s| s.trim_end_matches('/').to_string())
}

/// Cortex Gateway base URL: env → `infra.json` → `None`.
///
/// Env resolution prefers the new `CORTEX_GATEWAY_BASE_URL` / `GATEWAY_BASE_URL`
/// names but falls back to the legacy `HERMES_BASE_URL` so the user's existing
/// environment keeps working unchanged after the rebrand. The file value reads
/// `gateway_base_url` (with a `hermes_base_url` serde alias on `InfraConfig`).
pub fn gateway_base_url() -> Option<String> {
    let env_val = std::env::var("CORTEX_GATEWAY_BASE_URL")
        .or_else(|_| std::env::var("GATEWAY_BASE_URL"))
        .or_else(|_| std::env::var("HERMES_BASE_URL"))
        .ok();
    first_configured_url(env_val, load_file().gateway_base_url.as_deref())
}

/// Ollama server base URL: `OLLAMA_BASE_URL` → `infra.json` → `None`.
pub fn ollama_base_url() -> Option<String> {
    first_configured_url(
        std::env::var("OLLAMA_BASE_URL").ok(),
        load_file().ollama_base_url.as_deref(),
    )
}

/// Gitea host for AppImage self-update:
/// `CORTEX_UPDATE_GITEA_HOST` → `infra.json` → `None` (self-update idle).
pub fn update_gitea_host() -> Option<String> {
    first_configured_url(
        std::env::var("CORTEX_UPDATE_GITEA_HOST").ok(),
        load_file().update_gitea_host.as_deref(),
    )
}

/// Update-signing public key override (base64 of the raw 32-byte ed25519 key):
/// `CORTEX_UPDATE_PUBKEY` → `infra.json` → `None` (fall back to the baked key).
pub fn update_pubkey() -> Option<String> {
    first_configured(
        std::env::var("CORTEX_UPDATE_PUBKEY").ok(),
        load_file().update_pubkey.as_deref(),
    )
}

/// SSH target (`user@host`) for usage polling:
/// `CORTEX_USAGE_SSH_HOST` → `infra.json` → `None` (no SSH attempted).
pub fn usage_ssh_host() -> Option<String> {
    first_configured(
        std::env::var("CORTEX_USAGE_SSH_HOST").ok(),
        load_file().usage_ssh_host.as_deref(),
    )
}

/// Health-poller targets from `infra.json`. Entries with an empty source or
/// url are dropped. Empty vec → pollers no-op.
pub fn health_targets() -> Vec<HealthTargetEntry> {
    load_file()
        .health_targets
        .into_iter()
        .filter(|t| !t.source.trim().is_empty() && !t.url.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_over_file() {
        assert_eq!(
            first_configured(Some("http://env".into()), Some("http://file")),
            Some("http://env".to_string())
        );
    }

    #[test]
    fn file_used_when_env_unset_or_blank() {
        assert_eq!(
            first_configured(None, Some("http://file")),
            Some("http://file".to_string())
        );
        assert_eq!(
            first_configured(Some("   ".into()), Some("http://file")),
            Some("http://file".to_string())
        );
    }

    #[test]
    fn unconfigured_resolves_to_none() {
        assert_eq!(first_configured(None, None), None);
        assert_eq!(first_configured(Some("".into()), Some("  ")), None);
    }

    #[test]
    fn url_normalization_strips_trailing_slash() {
        assert_eq!(
            first_configured_url(None, Some("http://file:3000/")),
            Some("http://file:3000".to_string())
        );
    }

    #[test]
    fn missing_file_parses_to_default() {
        let cfg: InfraConfig = serde_json::from_str("{}").unwrap();
        assert!(cfg.gateway_base_url.is_none());
        assert!(cfg.usage_ssh_host.is_none());
        assert!(cfg.health_targets.is_empty());
    }

    #[test]
    fn parses_full_shape() {
        let cfg: InfraConfig = serde_json::from_str(
            r#"{
              "gateway_base_url": "http://gw:8642",
              "usage_ssh_host": "root@hv",
              "health_targets": [
                {"source": "gw", "url": "http://gw:8642/health"},
                {"source": "", "url": "http://dropped"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.gateway_base_url.as_deref(), Some("http://gw:8642"));
        assert_eq!(cfg.usage_ssh_host.as_deref(), Some("root@hv"));
        // health_targets() applies the empty-field filter; the raw parse keeps both.
        assert_eq!(cfg.health_targets.len(), 2);
        let filtered: Vec<_> = cfg
            .health_targets
            .into_iter()
            .filter(|t| !t.source.trim().is_empty() && !t.url.trim().is_empty())
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "gw");
    }

    #[test]
    fn garbage_json_degrades_to_default() {
        let cfg: InfraConfig =
            serde_json::from_str("not json").unwrap_or_default();
        assert!(cfg.gateway_base_url.is_none());
    }

    #[test]
    fn legacy_hermes_base_url_key_still_parses() {
        // Back-compat: an infra.json written before the Cortex Gateway rebrand
        // used `hermes_base_url`; the serde alias keeps it readable.
        let cfg: InfraConfig =
            serde_json::from_str(r#"{ "hermes_base_url": "http://legacy:8642" }"#).unwrap();
        assert_eq!(cfg.gateway_base_url.as_deref(), Some("http://legacy:8642"));
    }
}
