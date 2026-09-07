//! Lightweight self-update CHECK.
//!
//! This module does NOT download or install updates. It only:
//!   1. Fetches a small JSON manifest from a configurable URL.
//!   2. Loosely compares the manifest's `version` field with the baked-in
//!      `CARGO_PKG_VERSION`.
//!   3. Returns an `UpdateInfo` for the UI to display a "↑ update" pill.
//!
//! Manifest format (served by the configured gateway host):
//! ```json
//! { "version": "0.0.3", "notes": "fix usage tab", "url": "https://…/cortex-0.0.3.msi" }
//! ```
//!
//! Loose semver: we split on '.' and compare the first three components as
//! `u32`. Any non-numeric suffix (e.g. "0.1.0-rc.1") is stripped after the
//! first non-digit so we still get a usable ordering.
//!
//! Network/parse errors NEVER panic — they degrade to
//! `available: false, latest: current` so the UI stays calm.

use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub notes: Option<String>,
    pub url: Option<String>,
}

#[tauri::command]
pub async fn check_updates(manifest_url: String) -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();

    // Build a short-timeout client so the UI never hangs on a dead host.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("updater: client build failed: {e}");
            return Ok(unavailable(&current));
        }
    };

    let resp = match client.get(&manifest_url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::info!("updater: {} returned status {}", manifest_url, r.status());
            return Ok(unavailable(&current));
        }
        Err(e) => {
            tracing::info!("updater: fetch failed for {manifest_url}: {e}");
            return Ok(unavailable(&current));
        }
    };

    let val: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("updater: response parse failed: {e}");
            return Ok(unavailable(&current));
        }
    };

    // Accept EITHER a {version, notes, url} manifest OR a Gitea/GitHub releases
    // API array ([{tag_name, name, body, html_url, draft}, …], newest first).
    let (latest, notes, url) = if let Some(arr) = val.as_array() {
        let rel = arr
            .iter()
            .find(|r| !r.get("draft").and_then(|d| d.as_bool()).unwrap_or(false))
            .or_else(|| arr.first());
        match rel {
            Some(r) => (
                r.get("tag_name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
                r.get("body").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()).map(str::to_string),
                r.get("html_url").and_then(|v| v.as_str()).map(str::to_string),
            ),
            None => return Ok(unavailable(&current)),
        }
    } else {
        (
            val.get("version").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
            val.get("notes").and_then(|v| v.as_str()).map(str::to_string),
            val.get("url").and_then(|v| v.as_str()).map(str::to_string),
        )
    };

    if latest.is_empty() {
        return Ok(unavailable(&current));
    }
    let available = is_newer(&latest, &current);
    Ok(UpdateInfo { current, latest, available, notes, url })
}

fn unavailable(current: &str) -> UpdateInfo {
    UpdateInfo {
        current: current.to_string(),
        latest: current.to_string(),
        available: false,
        notes: None,
        url: None,
    }
}

/// Loose semver compare. Returns true iff `latest` > `current`.
/// Splits on '.' and parses each segment as `u32`, stopping at the first
/// non-digit run (so "0.1.0-rc.1" -> [0,1,0]).
fn is_newer(latest: &str, current: &str) -> bool {
    let a = parse_loose(latest);
    let b = parse_loose(current);
    // Compare segment-by-segment, treating missing trailing segments as 0 so
    // unequal-length versions compare numerically ("1.0" == "1.0.0") rather
    // than lexicographically (where the shorter vector would sort as older).
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn parse_loose(v: &str) -> Vec<u32> {
    // Wave 171 — strip the prerelease (`-rc.1`) and build (`+sha`) suffixes
    // BEFORE splitting on `.` so "0.1.0-rc.1" doesn't become [0,1,0,1] and
    // then sort as newer than [0,1,0]. The old per-segment digit-take only
    // dropped non-digits within a single segment, not the trailing rc.N
    // segments that come AFTER the `-`. Pre-existing bug; test was right.
    let cleaned = v.trim().trim_start_matches('v');
    let cleaned = cleaned
        .split_once('-')
        .map(|(head, _tail)| head)
        .unwrap_or(cleaned);
    let cleaned = cleaned
        .split_once('+')
        .map(|(head, _tail)| head)
        .unwrap_or(cleaned);
    cleaned
        .split('.')
        .map(|seg| {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_newer_patch() {
        assert!(is_newer("0.0.2", "0.0.1"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn rejects_equal_or_older() {
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(!is_newer("0.0.1", "0.0.2"));
        assert!(!is_newer("0.0.0", "0.0.1"));
    }

    #[test]
    fn handles_prerelease_suffix() {
        // "0.1.0-rc.1" parses as [0,1,0]; treated equal to "0.1.0".
        assert!(!is_newer("0.1.0-rc.1", "0.1.0"));
        assert!(is_newer("0.2.0-rc.1", "0.1.0"));
    }

    #[test]
    fn handles_v_prefix() {
        assert!(is_newer("v0.0.2", "0.0.1"));
        assert!(!is_newer("v0.0.1", "v0.0.1"));
    }

    #[test]
    fn unavailable_helper_marks_same_version() {
        let info = unavailable("0.0.1");
        assert!(!info.available);
        assert_eq!(info.current, info.latest);
    }
}
