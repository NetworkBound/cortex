//! Token/usage aggregation. Pulls per-session totals from the local span
//! event log and (optionally) polls the gateway for upstream credential_pool
//! status. Surfaced as the "Usage" tab in the Brain panel.

use crate::observability::tracing_store::TracingStore;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub total_tokens: u64,
    pub total_runs: u64,
    pub session_count: u64,
    pub by_session: Vec<SessionTokens>,
    pub by_provider: Vec<ProviderUsage>,
    /// Token/run totals attributed to the *effective model* each run used
    /// (e.g. `claude-sonnet-4-6`, `gpt-5.5`), independent of which adapter
    /// routed it. The breakdown that climbs as specific models are exercised.
    pub by_model: Vec<ModelUsage>,
    pub upstream_pool: Vec<UpstreamProviderStatus>,
    /// Latest Claude CLI rate-limit snapshot (from ~/.cortex/claude-usage.json,
    /// written by the claude_cli adapter). None when no recent activity.
    pub claude_limit: Option<ClaudeLimit>,
}

/// Claude CLI rate-limit window/status, deserialized from the JSON the
/// `claude_cli` adapter persists. Precise "tokens left" isn't exposed by the
/// CLI — only the rate-limit window + status, which is what we surface here.
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeLimit {
    pub status: Option<String>,
    pub resets_at: Option<i64>,
    pub rate_limit_type: Option<String>,
    pub out_of_credits: bool,
    pub is_using_overage: bool,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTokens {
    pub session_id: String,
    pub last_active_ms: i64,
    pub total_tokens: u64,
    pub runs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsage {
    pub agent_id: String,
    pub total_tokens: u64,
    pub runs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub model: String,
    /// The adapter that ran this model, when known (e.g. `gateway-remote`).
    pub agent_id: Option<String>,
    pub total_tokens: u64,
    pub runs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamProviderStatus {
    pub provider: String,
    pub label: Option<String>,
    pub status: String,           // "ready" | "exhausted" | "error" | "unknown"
    pub last_error_code: Option<i64>,
    pub last_error_message: Option<String>,
    pub request_count: Option<i64>,
    /// Epoch (seconds, float) of the last status update.
    pub last_status_at: Option<f64>,
    /// Epoch (seconds, float) when the last error's rate-limit window resets.
    pub last_error_reset_at: Option<f64>,
    /// Credential auth type (e.g. "oauth" / "api_key").
    pub auth_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayStatus {
    pub url: String,
    pub up: bool,
    pub model: Option<String>,
    pub features: Option<serde_json::Value>,
    pub latency_ms: Option<i64>,
}

pub async fn fetch_gateway_status(base_url: &str, api_key: &str) -> GatewayStatus {
    use std::time::Instant;
    let started = Instant::now();
    // Unconfigured gateway → calmly "down" with zero network I/O instead of
    // dialing a malformed URL.
    if base_url.trim().is_empty() {
        return GatewayStatus { url: String::new(), up: false, model: None, features: None, latency_ms: None };
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        // Client construction can fail (e.g. broken native TLS/cert store).
        // Report the gateway as down instead of panicking the async task.
        Err(_) => {
            return GatewayStatus {
                url: base_url.to_string(),
                up: false,
                model: None,
                features: None,
                latency_ms: Some(started.elapsed().as_millis() as i64),
            };
        }
    };

    // Probe /health first (no auth needed)
    let healthy = client
        .get(format!("{}/health", base_url))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if !healthy {
        return GatewayStatus {
            url: base_url.to_string(),
            up: false,
            model: None,
            features: None,
            latency_ms: Some(started.elapsed().as_millis() as i64),
        };
    }

    // Pull capabilities
    let cap_resp = client
        .get(format!("{}/v1/capabilities", base_url))
        .bearer_auth(api_key)
        .send()
        .await;
    let (model, features) = match cap_resp {
        Ok(r) => match r.json::<serde_json::Value>().await {
            Ok(v) => {
                let model = v.get("model").and_then(|m| m.as_str()).map(String::from);
                let features = v.get("features").cloned();
                (model, features)
            }
            Err(_) => (None, None),
        },
        Err(_) => (None, None),
    };

    GatewayStatus {
        url: base_url.to_string(),
        up: true,
        model,
        features,
        latency_ms: Some(started.elapsed().as_millis() as i64),
    }
}

pub fn build_summary(store: &TracingStore) -> anyhow::Result<UsageSummary> {
    let by_session = store.tokens_by_session(50).unwrap_or_default();
    let by_provider = store.tokens_by_provider(20).unwrap_or_default();
    let by_model = store.tokens_by_model(20).unwrap_or_default();

    let total_tokens: u64 = by_session.iter().map(|s| s.total_tokens).sum();
    let total_runs: u64 = by_session.iter().map(|s| s.runs).sum();
    let session_count = by_session.len() as u64;

    // fetch_upstream_pool_sync shells out to a blocking ssh (up to ~3s on an
    // unreachable host). build_summary is invoked from an async Tauri command,
    // so run the blocking work via block_in_place to avoid stalling a runtime
    // worker thread. Fall back to the direct call if we're not on a runtime.
    let upstream_pool = match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(fetch_upstream_pool_sync).unwrap_or_default(),
        Err(_) => fetch_upstream_pool_sync().unwrap_or_default(),
    };

    Ok(UsageSummary {
        total_tokens,
        total_runs,
        session_count,
        by_session,
        by_provider,
        by_model,
        upstream_pool,
        claude_limit: read_claude_limit(),
    })
}

/// Best-effort read of the Claude rate-limit snapshot at
/// `~/.cortex/claude-usage.json` (written by the claude_cli adapter).
/// Returns None if the file is missing or unparseable.
fn read_claude_limit() -> Option<ClaudeLimit> {
    let path = dirs::home_dir()?.join(".cortex/claude-usage.json");
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(ClaudeLimit {
        status: v.get("status").and_then(|s| s.as_str()).map(String::from),
        resets_at: v.get("resets_at").and_then(|r| r.as_i64()),
        rate_limit_type: v
            .get("rate_limit_type")
            .and_then(|r| r.as_str())
            .map(String::from),
        out_of_credits: v
            .get("out_of_credits")
            .and_then(|o| o.as_bool())
            .unwrap_or(false),
        is_using_overage: v
            .get("is_using_overage")
            .and_then(|o| o.as_bool())
            .unwrap_or(false),
        updated_ms: v.get("updated_ms").and_then(|u| u.as_i64()).unwrap_or(0),
    })
}

/// SSH to the configured hypervisor and read the Cortex Gateway's auth.json
/// to surface its credential_pool view: per-provider status, last error,
/// request count. The SSH target comes from `CORTEX_USAGE_SSH_HOST` /
/// `~/.cortex/infra.json` (`usage_ssh_host`); the caller's SSH key must
/// already be trusted on that host.
pub fn fetch_upstream_pool_sync() -> anyhow::Result<Vec<UpstreamProviderStatus>> {
    fetch_upstream_pool_with_host(crate::infra_config::usage_ssh_host())
}

/// Host-gated worker behind [`fetch_upstream_pool_sync`]. `None` means the
/// feature is unconfigured: return an empty pool WITHOUT spawning ssh — no
/// LAN dialing, no error, no log spam. The UI renders "no upstream data".
fn fetch_upstream_pool_with_host(
    host: Option<String>,
) -> anyhow::Result<Vec<UpstreamProviderStatus>> {
    let Some(host) = host else {
        return Ok(Vec::new());
    };
    let output = crate::sys::no_window("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=4")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(host)
        .arg("pct exec <gateway-ct> -- cat /home/gateway/.cortex-gateway/auth.json")
        .output()?;
    if !output.status.success() {
        anyhow::bail!("ssh failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    parse_credential_pool(&json)
}

/// Pure JSON → status-row mapping, split out so it's unit-testable without
/// any SSH.
fn parse_credential_pool(
    json: &serde_json::Value,
) -> anyhow::Result<Vec<UpstreamProviderStatus>> {
    let pool = json
        .get("credential_pool")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for (provider, val) in pool {
        let arr = val.as_array().cloned().unwrap_or_default();
        for entry in arr {
            let status = entry
                .get("last_status")
                .and_then(|s| s.as_str())
                .map(String::from)
                .unwrap_or_else(|| "ready".to_string());
            let last_error_code = entry
                .get("last_error_code")
                .and_then(|c| c.as_i64());
            let last_error_message = entry
                .get("last_error_message")
                .and_then(|m| m.as_str())
                .map(|s| s.chars().take(160).collect::<String>());
            let request_count = entry
                .get("request_count")
                .and_then(|c| c.as_i64());
            let label = entry
                .get("label")
                .and_then(|l| l.as_str())
                .map(String::from);
            let last_status_at = entry
                .get("last_status_at")
                .and_then(|t| t.as_f64());
            let last_error_reset_at = entry
                .get("last_error_reset_at")
                .and_then(|t| t.as_f64());
            let auth_type = entry
                .get("auth_type")
                .and_then(|a| a.as_str())
                .map(String::from);
            out.push(UpstreamProviderStatus {
                provider: provider.clone(),
                label,
                status,
                last_error_code,
                last_error_message,
                request_count,
                last_status_at,
                last_error_reset_at,
                auth_type,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_ssh_host_is_a_quiet_no_op() {
        // No host → empty pool, Ok, and crucially no ssh process spawned.
        let out = fetch_upstream_pool_with_host(None).expect("must not error");
        assert!(out.is_empty());
    }

    #[test]
    fn parses_credential_pool_rows() {
        let json = serde_json::json!({
            "credential_pool": {
                "anthropic": [
                    {"last_status": "ready", "request_count": 5, "label": "main"},
                    {"last_status": "exhausted", "last_error_code": 429}
                ]
            }
        });
        let rows = parse_credential_pool(&json).expect("parse");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider == "anthropic"));
        assert!(rows.iter().any(|r| r.status == "exhausted"
            && r.last_error_code == Some(429)));
    }

    #[test]
    fn missing_pool_key_parses_to_empty() {
        let rows = parse_credential_pool(&serde_json::json!({})).expect("parse");
        assert!(rows.is_empty());
    }
}
