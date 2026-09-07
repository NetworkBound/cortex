//! Infrastructure health pollers — periodically probe the HTTP endpoints
//! configured in `~/.cortex/infra.json` (`health_targets`) and write samples
//! to the tracing store. Results are surfaced by
//! `commands::observability::homelab_health` and rendered as the status
//! strip in the observability panel.
//!
//! Shipped builds contain NO baked-in probe targets: when `health_targets`
//! is absent or empty the loop idles without dialing anything (and without
//! logging errors), so a standalone install never spams a LAN it isn't on.
//! The target list is re-read every tick, so adding/editing the config file
//! takes effect within one interval — no restart needed.

use crate::infra_config;
use crate::observability::tracing_store::TracingStore;
use serde::Serialize;
use std::time::{Duration, Instant};
use tauri::Manager;

#[derive(Debug, Clone, Serialize)]
pub struct HealthTarget {
    pub source: String,
    pub url: String,
    pub kind: &'static str,
}

/// Probe targets from `~/.cortex/infra.json`. Empty when unconfigured —
/// the caller must treat that as "feature off" and do no network I/O.
pub fn configured_targets() -> Vec<HealthTarget> {
    infra_config::health_targets()
        .into_iter()
        .map(|t| HealthTarget { source: t.source, url: t.url, kind: "http" })
        .collect()
}

pub fn spawn_pollers(app: tauri::AppHandle) {
    // Use Tauri's async_runtime so this works when called from `setup()`,
    // which runs before a tokio runtime context is otherwise active.
    tauri::async_runtime::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest");
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tick.tick().await;
            // Re-read each tick so config edits apply live. Unconfigured →
            // quiet no-op: no probes, no log spam, no recorded failures.
            let targets = configured_targets();
            if targets.is_empty() {
                continue;
            }
            let store = match app.try_state::<TracingStore>() {
                Some(s) => s,
                None => continue,
            };
            for target in targets {
                let started = Instant::now();
                let res = client.get(&target.url).send().await;
                let latency = started.elapsed().as_millis() as i64;
                let (ok, payload) = match res {
                    Ok(r) => {
                        let status = r.status().as_u16();
                        let ok = (200..400).contains(&status);
                        (ok, format!("{{\"status\":{status}}}"))
                    }
                    Err(e) => {
                        let payload = serde_json::json!({ "error": e.to_string() }).to_string();
                        (false, payload)
                    }
                };
                let _ = store.record_health(&target.source, ok, Some(latency), Some(&payload));
            }
        }
    });
}
