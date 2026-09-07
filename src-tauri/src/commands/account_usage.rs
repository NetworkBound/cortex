//! Live account-usage polling for the two paid subscriptions Cortex drives:
//! Claude (Max) and ChatGPT/Codex (Plus). Both are best-effort and independent
//! — a missing token, expired credential, network blip, or powered-off homelab
//! must never error the whole command; the affected provider just comes back
//! `None` and the UI renders a "not connected" placeholder.
//!
//! Claude usage is polled directly over local HTTP (`api.anthropic.com`) using
//! the OAuth token in `~/.claude/.credentials.json`.
//!
//! ChatGPT usage is polled by SSHing to CT154 (the gateway box) and running a
//! remote python one-liner there. The Codex token NEVER leaves CT154 — only the
//! resulting usage JSON travels back over SSH.

use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Minimum spacing between real upstream fetches per provider. The UI polls
/// `account_usage` every ~60s, but the Anthropic `/api/oauth/usage` endpoint
/// rate-limits (429) under that cadence — so we only hit upstream this often and
/// serve a cached value in between. On a failed fetch we keep serving the last
/// good value (and reset the timer) so the card never blanks on a transient 429.
const USAGE_TTL: Duration = Duration::from_secs(240);

fn claude_cache() -> &'static Mutex<Option<(ClaudeUsage, Instant)>> {
    static C: OnceLock<Mutex<Option<(ClaudeUsage, Instant)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn chatgpt_cache() -> &'static Mutex<Option<(ChatgptUsage, Instant)>> {
    static C: OnceLock<Mutex<Option<(ChatgptUsage, Instant)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountUsage {
    pub claude: Option<ClaudeUsage>,
    pub chatgpt: Option<ChatgptUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaudeUsage {
    pub five_hour_pct: f64,
    pub five_hour_resets_at: Option<String>,
    pub seven_day_pct: f64,
    pub seven_day_resets_at: Option<String>,
    pub sonnet_pct: Option<f64>,
    pub extra_monthly_limit: Option<f64>,
    pub extra_used_credits: Option<f64>,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatgptUsage {
    pub plan_type: String,
    pub primary_used_pct: f64,
    pub primary_reset_at: i64,
    pub secondary_used_pct: f64,
    pub secondary_reset_at: i64,
    pub limit_reached: bool,
    pub credits_balance: Option<String>,
}

#[tauri::command]
pub async fn account_usage() -> Result<AccountUsage, String> {
    // Both halves are independent and best-effort. Claude is async HTTP;
    // ChatGPT is a blocking SSH call wrapped in block_in_place (mirroring
    // usage.rs::build_summary). Run Claude first, then ChatGPT — neither can
    // fail the command.
    let claude = claude_usage_cached().await;
    let chatgpt = chatgpt_usage_cached();
    Ok(AccountUsage { claude, chatgpt })
}

/// Claude usage with TTL caching + serve-stale-on-error, so a 429 from the
/// rate-limited `/api/oauth/usage` endpoint doesn't blank the card.
async fn claude_usage_cached() -> Option<ClaudeUsage> {
    {
        let g = claude_cache().lock().ok()?;
        if let Some((ref v, ts)) = *g {
            if ts.elapsed() < USAGE_TTL {
                return Some(v.clone());
            }
        }
    }
    match fetch_claude_usage().await {
        Some(v) => {
            if let Ok(mut g) = claude_cache().lock() {
                *g = Some((v.clone(), Instant::now()));
            }
            Some(v)
        }
        None => {
            // Failed (likely 429). Keep serving the last good value and reset its
            // timer so we back off the rate-limited endpoint for another TTL.
            let mut g = claude_cache().lock().ok()?;
            if let Some((v, ts)) = g.as_mut() {
                *ts = Instant::now();
                return Some(v.clone());
            }
            None
        }
    }
}

/// ChatGPT usage with the same TTL cache (cuts SSH load + survives a transient
/// homelab/SSH blip without blanking the card).
fn chatgpt_usage_cached() -> Option<ChatgptUsage> {
    {
        if let Ok(g) = chatgpt_cache().lock() {
            if let Some((ref v, ts)) = *g {
                if ts.elapsed() < USAGE_TTL {
                    return Some(v.clone());
                }
            }
        }
    }
    let fresh = match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(fetch_chatgpt_usage_sync),
        Err(_) => fetch_chatgpt_usage_sync(),
    };
    match fresh {
        Some(v) => {
            if let Ok(mut g) = chatgpt_cache().lock() {
                *g = Some((v.clone(), Instant::now()));
            }
            Some(v)
        }
        None => {
            let mut g = chatgpt_cache().lock().ok()?;
            if let Some((v, ts)) = g.as_mut() {
                *ts = Instant::now();
                return Some(v.clone());
            }
            None
        }
    }
}

/// Read the Claude OAuth access token from `~/.claude/.credentials.json`.
/// Returns None if the file is missing or the token isn't present.
fn read_claude_token() -> Option<String> {
    let path = dirs::home_dir()?.join(".claude/.credentials.json");
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
        .map(String::from)
}

/// Poll `api.anthropic.com/api/oauth/usage` for the live Claude rate-limit
/// utilization. Any failure (no token, 401, network, parse) → None.
async fn fetch_claude_usage() -> Option<ClaudeUsage> {
    let token = read_claude_token()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;

    // Helpers tolerate null sub-objects gracefully.
    let pct = |key: &str| -> f64 {
        v.get(key)
            .and_then(|o| o.get("utilization"))
            .and_then(|u| u.as_f64())
            .unwrap_or(0.0)
    };
    let reset = |key: &str| -> Option<String> {
        v.get(key)
            .and_then(|o| o.get("resets_at"))
            .and_then(|r| r.as_str())
            .map(String::from)
    };

    let extra = v.get("extra_usage");
    let extra_monthly_limit = extra
        .and_then(|e| e.get("monthly_limit"))
        .and_then(|m| m.as_f64());
    let extra_used_credits = extra
        .and_then(|e| e.get("used_credits"))
        .and_then(|c| c.as_f64());
    let currency = extra
        .and_then(|e| e.get("currency"))
        .and_then(|c| c.as_str())
        .map(String::from);

    // sonnet utilization is its own sub-object; surface it only when present.
    let sonnet_pct = v
        .get("seven_day_sonnet")
        .and_then(|o| o.get("utilization"))
        .and_then(|u| u.as_f64());

    Some(ClaudeUsage {
        five_hour_pct: pct("five_hour"),
        five_hour_resets_at: reset("five_hour"),
        seven_day_pct: pct("seven_day"),
        seven_day_resets_at: reset("seven_day"),
        sonnet_pct,
        extra_monthly_limit,
        extra_used_credits,
        currency,
    })
}

/// SSH to the configured hypervisor (`CORTEX_USAGE_SSH_HOST` /
/// `~/.cortex/infra.json` `usage_ssh_host`) and run a remote python one-liner
/// that reads the Codex token from `/home/gateway/.cortex-gateway/auth.json`, queries
/// `chatgpt.com/backend-api/codex/usage`, and prints ONLY the usage JSON. The
/// token never crosses SSH — only the JSON response does. No host configured →
/// None immediately (no ssh spawned, no LAN dialing); the UI shows the
/// provider as "not connected". Any failure (host down, no key, expired
/// token, parse) → None.
fn fetch_chatgpt_usage_sync() -> Option<ChatgptUsage> {
    fetch_chatgpt_usage_with_host(crate::infra_config::usage_ssh_host())
}

/// Host-gated worker behind [`fetch_chatgpt_usage_sync`], split out so the
/// unconfigured → no-op path is unit-testable without any SSH.
fn fetch_chatgpt_usage_with_host(host: Option<String>) -> Option<ChatgptUsage> {
    let host = host?;
    // Remote one-liner. Keep it self-contained (stdlib only) so it runs on a
    // bare python3. urllib is used to avoid a requests dependency on CT154.
    const REMOTE_PY: &str = "import json,urllib.request;\
d=json.load(open('/home/gateway/.cortex-gateway/auth.json'));\
t=d['credential_pool']['openai-codex'][0]['access_token'];\
r=urllib.request.Request('https://chatgpt.com/backend-api/codex/usage',\
headers={'Authorization':'Bearer '+t,'User-Agent':'codex-cli'});\
print(urllib.request.urlopen(r,timeout=3).read().decode())";

    // The app has SSH key access to the hypervisor, NOT directly to the
    // gateway container. Route through the host and `pct exec <gateway-ct>`. Single-
    // quote the python so the remote shell forwards it as one argument to
    // python3 -c.
    let remote_py_quoted = format!("'{}'", REMOTE_PY.replace('\'', "'\\''"));
    let remote_cmd = format!("pct exec <gateway-ct> -- python3 -c {remote_py_quoted}");
    let output = crate::sys::no_window("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=4")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(host)
        .arg(remote_cmd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

    let plan_type = v
        .get("plan_type")
        .and_then(|p| p.as_str())
        .unwrap_or("unknown")
        .to_string();

    let rate = v.get("rate_limit");
    let limit_reached = rate
        .and_then(|r| r.get("limit_reached"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    let window_pct = |name: &str| -> f64 {
        rate.and_then(|r| r.get(name))
            .and_then(|w| w.get("used_percent"))
            .and_then(|u| u.as_f64())
            .unwrap_or(0.0)
    };
    let window_reset = |name: &str| -> i64 {
        rate.and_then(|r| r.get(name))
            .and_then(|w| w.get("reset_at"))
            .and_then(|u| u.as_i64())
            .unwrap_or(0)
    };

    let credits_balance = v
        .get("credits")
        .and_then(|c| c.get("balance"))
        .and_then(|b| b.as_str())
        .map(String::from);

    Some(ChatgptUsage {
        plan_type,
        primary_used_pct: window_pct("primary_window"),
        primary_reset_at: window_reset("primary_window"),
        secondary_used_pct: window_pct("secondary_window"),
        secondary_reset_at: window_reset("secondary_window"),
        limit_reached,
        credits_balance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_ssh_host_yields_none_without_dialing() {
        // No host configured → None immediately; the UI shows "not connected"
        // and no ssh process is ever spawned.
        assert!(fetch_chatgpt_usage_with_host(None).is_none());
    }
}
