//! Resilient Cortex Gateway endpoint resolution.
//!
//! Cortex must reach the Cortex Gateway "no matter the connection": on the LAN
//! it should use the fast local IP, but when the device leaves the LAN it must
//! fail over to a Tailscale MagicDNS name or a public tunnel without any user
//! action. This module probes an ordered list of candidate base URLs and keeps
//! [`crate::app_state::Config::gateway_base_url`] pointed at the first reachable
//! one.
//!
//! Design notes:
//! - Every probe is time-boxed (`PROBE_TIMEOUT`) so a dead candidate can never
//!   stall startup or the re-resolve loop.
//! - Resolution is best-effort: if nothing is reachable we leave the current
//!   value untouched (better a stale-but-maybe-recovering URL than an empty one)
//!   and never panic.
//! - The candidate order encodes preference, so we always prefer the LAN entry
//!   when it's up and only fall through to slower remote entries when it isn't.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use crate::app_state::Config;

/// Per-candidate health-probe timeout. Short enough that walking a list of dead
/// candidates stays snappy, long enough to tolerate a sluggish tunnel hop.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(2500);

/// How often the background loop re-checks the candidate list for a better
/// (more-preferred and reachable) endpoint than the one currently in use.
pub const RESOLVE_INTERVAL: Duration = Duration::from_secs(60);

/// Probe each candidate's `GET <url>/health` IN ORDER with a short timeout and
/// return the first that answers with a 2xx status. Returns `None` when every
/// candidate is unreachable or the HTTP client can't even be constructed.
///
/// The order of `candidates` is significant: it's the preference order, so the
/// first reachable URL wins (LAN before tailnet before public tunnel).
pub async fn resolve_gateway_base_url(candidates: &[String]) -> Option<String> {
    // A per-request timeout is also set below; the client-level timeout is a
    // belt-and-suspenders cap that also covers connect/TLS setup.
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;

    for candidate in candidates {
        let base = candidate.trim_end_matches('/');
        if base.is_empty() {
            continue;
        }
        let url = format!("{base}/health");
        match client.get(&url).timeout(PROBE_TIMEOUT).send().await {
            Ok(resp) if resp.status().is_success() => {
                return Some(base.to_string());
            }
            Ok(_) | Err(_) => continue,
        }
    }
    None
}

/// Run the failover loop forever: probe on startup and then every
/// [`RESOLVE_INTERVAL`], updating `config.gateway_base_url` whenever the most
/// preferred reachable candidate differs from the current value.
///
/// This is intended to be `tokio::spawn`ed from app setup. It never blocks the
/// caller, never panics, and is a no-op (beyond probing) when nothing changes.
/// If the candidate list is empty there's nothing to fail over to, so it
/// returns immediately.
pub async fn run_failover_loop(candidates: Vec<String>, config: Arc<RwLock<Config>>) {
    if candidates.is_empty() {
        return;
    }
    loop {
        if let Some(reachable) = resolve_gateway_base_url(&candidates).await {
            // Read the current value under a short-lived lock, decide outside
            // the lock, then take the write lock only if a change is needed.
            let current = config.read().gateway_base_url.clone();
            if reachable != current {
                config.write().gateway_base_url = reachable.clone();
                tracing::info!(
                    from = %current,
                    to = %reachable,
                    "gateway endpoint failover: switched to a reachable candidate"
                );
            }
        } else {
            // Nothing reachable — keep the current value and try again later.
            tracing::debug!("gateway failover: no candidate reachable; keeping current base url");
        }
        tokio::time::sleep(RESOLVE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_candidates_resolve_to_none() {
        assert_eq!(resolve_gateway_base_url(&[]).await, None);
    }

    #[tokio::test]
    async fn unreachable_candidates_resolve_to_none() {
        // Reserved TEST-NET-1 address that should never answer, with a port
        // that's closed. The short timeout keeps this test fast.
        let candidates = vec!["http://192.0.2.1:9".to_string()];
        assert_eq!(resolve_gateway_base_url(&candidates).await, None);
    }

    #[tokio::test]
    async fn empty_loop_returns_immediately() {
        // Should not hang despite the infinite loop, because the candidate list
        // is empty.
        let cfg = Arc::new(RwLock::new(Config::default()));
        run_failover_loop(vec![], cfg).await;
    }
}
