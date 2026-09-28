//! Quota-aware failover for chat turns.
//!
//! When the picked agent is out of quota (Claude's own `rate_limit_event`
//! says `rejected`, or a usage percentage crosses the configured threshold)
//! or a run dies with a rate-limit / transient error BEFORE streaming any
//! output, `commands::chat::chat_send` re-dispatches the same turn ONCE to the
//! next capable agent in a user-ordered chain and records why, so the routing
//! reason in Run Replay reads "failed over from claude-cli: quota (…)".
//!
//! Everything here is pure or a small JSON file read: the decision
//! ([`should_failover`]), the chain walk ([`pick_fallback_agent`]), the
//! policy file (`~/.cortex/failover.json`, DEFAULT-OFF) and the Claude usage
//! snapshot read from `~/.cortex/claude-usage.json` (written by
//! `agents::local_cli` on every Claude run).
//!
//! Error classification is shared with the helper-feature fallback chain
//! (`agents::oneshot::classify_error`), extended with the quota vocabulary
//! (`429`, "rate limit", "usage limit", "hit your limit", …) that must always
//! count — `classify_error` treats unknown text as Permanent on purpose.

use crate::agents::oneshot::{classify_error, ErrorClass};
use crate::agents::{AgentCapability, Registry};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Default usage threshold: fail over once a window is ≥ this % used.
pub const DEFAULT_THRESHOLD_PCT: f64 = 95.0;

/// A `claude-usage.json` snapshot older than this is ignored.
const SNAPSHOT_MAX_AGE_MS: i64 = 6 * 60 * 60 * 1000;

/// On-disk schema for `~/.cortex/failover.json`. Missing file / malformed
/// JSON ⇒ `Default` ⇒ `enabled: false` ⇒ chat behaves exactly as today.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FailoverPolicy {
    pub enabled: bool,
    /// Ordered fallback agent ids (registry ids: `codex-cli`, `gemini-cli`,
    /// `ollama`, `gateway-remote`, …). The primary is skipped if listed.
    pub chain: Vec<String>,
    /// Fail over pre-dispatch when a usage window is at/over this percent.
    pub threshold_pct: f64,
    /// Also fail over on non-quota transient errors (5xx, network, timeout)
    /// that occur before any output.
    pub on_transient_error: bool,
    /// Treat the CLI's soft `allowed_warning` status as exhausted.
    pub on_warning: bool,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            chain: Vec::new(),
            threshold_pct: DEFAULT_THRESHOLD_PCT,
            on_transient_error: true,
            on_warning: false,
        }
    }
}

impl FailoverPolicy {
    /// Trim/de-dup the chain and clamp the threshold into 1..=100. Applied
    /// on write and on load so a hand-edited file can't produce a 0% (always)
    /// or 1000% (never) threshold by accident.
    pub fn normalized(mut self) -> Self {
        let mut seen: Vec<String> = Vec::new();
        for id in self
            .chain
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            if !seen.iter().any(|s| s == id) {
                seen.push(id.to_string());
            }
        }
        self.chain = seen;
        if !self.threshold_pct.is_finite() {
            self.threshold_pct = DEFAULT_THRESHOLD_PCT;
        }
        self.threshold_pct = self.threshold_pct.clamp(1.0, 100.0);
        self
    }

    /// Enabled AND has somewhere to go.
    pub fn is_armed(&self) -> bool {
        self.enabled && self.chain.iter().any(|s| !s.trim().is_empty())
    }
}

/// What is known about one agent's remaining quota right now. All fields are
/// optional signals; an empty snapshot never triggers a failover.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    /// Highest known used-percentage across the provider's windows.
    pub used_pct: Option<f64>,
    /// Label of the window `used_pct` came from (`"5h"`, `"7d"`, …).
    pub window: Option<String>,
    /// The provider says the limit is reached / credits are exhausted.
    pub limit_reached: bool,
    /// The provider raised its soft "approaching the limit" warning.
    pub warning: bool,
}

/// Why a turn was (or would be) failed over. `Display` is the short label
/// embedded in the routing reason.
#[derive(Debug, Clone, PartialEq)]
pub enum FailoverReason {
    /// A usage window is at/over the policy threshold.
    Quota { pct: f64, window: String },
    /// The provider's own soft warning (`allowed_warning`) and `on_warning`.
    Warning,
    /// The provider reports the limit reached / out of credits.
    LimitReached,
    /// The run failed with a rate-limit / quota error before any output.
    RateLimited,
    /// The run failed with another transient error before any output.
    Transient,
}

impl fmt::Display for FailoverReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FailoverReason::Quota { pct, window } => {
                write!(f, "quota ({window} window at {pct:.0}%)")
            }
            FailoverReason::Warning => write!(f, "quota (provider usage warning)"),
            FailoverReason::LimitReached => write!(f, "quota (limit reached)"),
            FailoverReason::RateLimited => write!(f, "quota (rate-limited)"),
            FailoverReason::Transient => write!(f, "transient error"),
        }
    }
}

/// Quota/rate-limit vocabulary across the CLIs and HTTP adapters. Checked
/// BEFORE `classify_error`, whose unknown ⇒ Permanent default would otherwise
/// swallow e.g. Claude Code's "You've hit your limit".
pub fn is_quota_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const HINTS: &[&str] = &[
        "429",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "quota",
        "usage limit",
        "hit your limit",
        "limit reached",
        "limit_reached",
        "out of credits",
        "insufficient credits",
        "credit balance",
        "overloaded",
        "over capacity",
    ];
    HINTS.iter().any(|h| m.contains(h))
}

/// The decision. Pure.
///
/// * `error_text: Some(..)` — a run already failed (before any output) with
///   this message: fail over on quota/rate-limit text, or on any other
///   `Transient` error when `policy.on_transient_error`; never on a
///   `Permanent` one (auth, bad request, model not found — the next agent
///   would not help and the user needs to see it).
/// * `error_text: None` — pre-dispatch: consult `usage` only.
///
/// Always `None` when the policy is off or has an empty chain.
pub fn should_failover(
    error_text: Option<&str>,
    usage: Option<&UsageSnapshot>,
    policy: &FailoverPolicy,
) -> Option<FailoverReason> {
    if !policy.is_armed() {
        return None;
    }
    if let Some(err) = error_text.map(str::trim).filter(|s| !s.is_empty()) {
        if is_quota_error(err) {
            return Some(FailoverReason::RateLimited);
        }
        return match classify_error(err) {
            ErrorClass::Transient if policy.on_transient_error => Some(FailoverReason::Transient),
            _ => None,
        };
    }
    let u = usage?;
    if u.limit_reached {
        return Some(FailoverReason::LimitReached);
    }
    if u.warning && policy.on_warning {
        return Some(FailoverReason::Warning);
    }
    let threshold = policy.threshold_pct.clamp(1.0, 100.0);
    match u.used_pct {
        Some(pct) if pct.is_finite() && pct >= threshold => Some(FailoverReason::Quota {
            pct,
            window: u.window.clone().unwrap_or_else(|| "usage".to_string()),
        }),
        _ => None,
    }
}

/// First chain entry that is not in `exclude` (the primary + anything that
/// already failed this turn), is registered, reports available, hasn't been
/// observed unreachable by the health poll, and can chat.
pub fn pick_fallback_agent(
    policy: &FailoverPolicy,
    registry: &Registry,
    exclude: &[&str],
) -> Option<String> {
    let descriptors = registry.list_descriptors();
    policy
        .chain
        .iter()
        .map(|s| s.trim())
        .filter(|id| !id.is_empty() && !exclude.contains(id))
        .find(|id| {
            descriptors.iter().any(|d| {
                d.id == *id
                    && d.available
                    && d.capabilities.contains(&AgentCapability::Chat)
                    && registry.known_reachable(id).unwrap_or(true)
            })
        })
        .map(str::to_string)
}

/// The routing-reason fragment recorded for a failover, e.g.
/// `failed over from claude-cli: quota (limit reached) → codex-cli`.
pub fn describe(from: &str, to: &str, why: &FailoverReason) -> String {
    format!("failed over from {from}: {why} → {to}")
}

// ─────────────── Policy file: ~/.cortex/failover.json ───────────────

/// `~/.cortex/failover.json`.
pub fn failover_path() -> Option<std::path::PathBuf> {
    crate::paths::cortex_dir().map(|c| c.join("failover.json"))
}

/// Parse the policy file body. Malformed JSON ⇒ default (OFF) — like
/// outcome routing, the only safe failure mode for a routing preference is
/// "behave exactly like today". Pure.
pub(crate) fn parse_failover_policy(raw: &str) -> FailoverPolicy {
    serde_json::from_str::<FailoverPolicy>(raw)
        .unwrap_or_default()
        .normalized()
}

/// Read the policy; re-read per `chat_send` so edits apply on the next turn.
pub fn load_failover_policy() -> FailoverPolicy {
    let Some(path) = failover_path() else {
        return FailoverPolicy::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_failover_policy(&raw),
        Err(_) => FailoverPolicy::default(),
    }
}

/// Persist the policy (normalized), creating `~/.cortex/` if needed.
pub fn write_failover_policy(policy: &FailoverPolicy) -> anyhow::Result<FailoverPolicy> {
    let path = failover_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let policy = policy.clone().normalized();
    let body = serde_json::to_string_pretty(&policy)?;
    std::fs::write(&path, body)?;
    Ok(policy)
}

// ─────────────── Usage snapshots ───────────────

/// Build a [`UsageSnapshot`] from the body of `~/.cortex/claude-usage.json`
/// (`{ status, resets_at, out_of_credits, updated_ms, … }`, see
/// `local_cli::persist_claude_limit`). `None` when the snapshot is stale:
/// older than [`SNAPSHOT_MAX_AGE_MS`], or its window has already reset
/// (`resets_at`, seconds or millis, in the past). Pure.
pub(crate) fn parse_claude_limit_snapshot(raw: &str, now_ms: i64) -> Option<UsageSnapshot> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let updated_ms = v.get("updated_ms").and_then(|u| u.as_i64()).unwrap_or(0);
    if updated_ms <= 0 || now_ms.saturating_sub(updated_ms) > SNAPSHOT_MAX_AGE_MS {
        return None;
    }
    if let Some(resets) = v.get("resets_at").and_then(|r| r.as_i64()) {
        // Claude reports epoch seconds; tolerate millis too.
        let resets_ms = if resets < 100_000_000_000 {
            resets.saturating_mul(1000)
        } else {
            resets
        };
        if resets_ms > 0 && now_ms >= resets_ms {
            return None;
        }
    }
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let out_of_credits = v
        .get("out_of_credits")
        .and_then(|o| o.as_bool())
        .unwrap_or(false);
    Some(UsageSnapshot {
        used_pct: None,
        window: v
            .get("rate_limit_type")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        limit_reached: status == "rejected" || out_of_credits,
        warning: status == "allowed_warning",
    })
}

/// The current usage snapshot for `agent_id`, if Cortex has one. Today only
/// the local Claude CLI leaves a machine-readable trail (its own
/// `rate_limit_event`s); other agents return `None` and are only ever failed
/// over on an actual error.
pub fn usage_snapshot_for(agent_id: &str) -> Option<UsageSnapshot> {
    if agent_id != "claude-cli" {
        return None;
    }
    let path = crate::paths::cortex_dir()?.join("claude-usage.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    parse_claude_limit_snapshot(&raw, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentAdapter, AgentDescriptor, AgentEvent, ChatRequest};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn armed(chain: &[&str]) -> FailoverPolicy {
        FailoverPolicy {
            enabled: true,
            chain: chain.iter().map(|s| s.to_string()).collect(),
            ..FailoverPolicy::default()
        }
    }

    #[test]
    fn off_or_empty_chain_never_fails_over() {
        let usage = UsageSnapshot {
            limit_reached: true,
            ..Default::default()
        };
        let off = FailoverPolicy {
            chain: vec!["codex-cli".into()],
            ..FailoverPolicy::default()
        };
        assert_eq!(
            should_failover(Some("429 rate limit"), Some(&usage), &off),
            None
        );
        let empty = FailoverPolicy {
            enabled: true,
            chain: vec!["  ".into()],
            ..FailoverPolicy::default()
        };
        assert_eq!(
            should_failover(Some("429 rate limit"), Some(&usage), &empty),
            None
        );
        assert!(!empty.is_armed());
        assert!(armed(&["codex-cli"]).is_armed());
    }

    #[test]
    fn error_text_decision_table() {
        let p = armed(&["codex-cli"]);
        assert_eq!(
            should_failover(Some("You've hit your limit · resets 3pm"), None, &p),
            Some(FailoverReason::RateLimited)
        );
        assert_eq!(
            should_failover(Some("HTTP 429 Too Many Requests"), None, &p),
            Some(FailoverReason::RateLimited)
        );
        assert_eq!(
            should_failover(Some("upstream 503 service unavailable"), None, &p),
            Some(FailoverReason::Transient)
        );
        // Permanent errors never fail over.
        assert_eq!(
            should_failover(Some("401 unauthorized: invalid api key"), None, &p),
            None
        );
        assert_eq!(
            should_failover(Some("model not found: gpt-9"), None, &p),
            None
        );
        // Unknown text is Permanent (conservative) — no failover.
        assert_eq!(
            should_failover(Some("something odd happened"), None, &p),
            None
        );
        // Transient failover can be switched off; quota text still counts.
        let quota_only = FailoverPolicy {
            on_transient_error: false,
            ..armed(&["codex-cli"])
        };
        assert_eq!(
            should_failover(Some("503 service unavailable"), None, &quota_only),
            None
        );
        assert_eq!(
            should_failover(Some("rate limit"), None, &quota_only),
            Some(FailoverReason::RateLimited)
        );
        // Empty error text falls through to the usage check (none here).
        assert_eq!(should_failover(Some("  "), None, &p), None);
    }

    #[test]
    fn usage_decision_table() {
        let p = armed(&["codex-cli"]);
        assert_eq!(should_failover(None, None, &p), None);
        assert_eq!(
            should_failover(None, Some(&UsageSnapshot::default()), &p),
            None
        );
        let reached = UsageSnapshot {
            limit_reached: true,
            ..Default::default()
        };
        assert_eq!(
            should_failover(None, Some(&reached), &p),
            Some(FailoverReason::LimitReached)
        );
        let warn = UsageSnapshot {
            warning: true,
            ..Default::default()
        };
        assert_eq!(should_failover(None, Some(&warn), &p), None);
        let p_warn = FailoverPolicy {
            on_warning: true,
            ..armed(&["codex-cli"])
        };
        assert_eq!(
            should_failover(None, Some(&warn), &p_warn),
            Some(FailoverReason::Warning)
        );
        let pct = |v: f64| UsageSnapshot {
            used_pct: Some(v),
            window: Some("5h".into()),
            ..Default::default()
        };
        assert_eq!(should_failover(None, Some(&pct(94.9)), &p), None);
        assert_eq!(
            should_failover(None, Some(&pct(97.0)), &p),
            Some(FailoverReason::Quota {
                pct: 97.0,
                window: "5h".into()
            })
        );
        assert_eq!(
            FailoverReason::Quota {
                pct: 97.0,
                window: "5h".into()
            }
            .to_string(),
            "quota (5h window at 97%)"
        );
        assert_eq!(
            describe("claude-cli", "codex-cli", &FailoverReason::LimitReached),
            "failed over from claude-cli: quota (limit reached) → codex-cli"
        );
    }

    #[test]
    fn normalize_dedups_and_clamps() {
        let p = FailoverPolicy {
            enabled: true,
            chain: vec![
                " codex-cli ".into(),
                "".into(),
                "codex-cli".into(),
                "ollama".into(),
            ],
            threshold_pct: 0.0,
            ..FailoverPolicy::default()
        }
        .normalized();
        assert_eq!(p.chain, vec!["codex-cli", "ollama"]);
        assert_eq!(p.threshold_pct, 1.0);
        let p = FailoverPolicy {
            threshold_pct: f64::NAN,
            ..FailoverPolicy::default()
        }
        .normalized();
        assert_eq!(p.threshold_pct, DEFAULT_THRESHOLD_PCT);
    }

    #[test]
    fn policy_file_round_trip_and_bad_json_is_off() {
        assert_eq!(
            parse_failover_policy("{not json"),
            FailoverPolicy::default()
        );
        assert!(!parse_failover_policy("").enabled);
        // Partial file: missing fields take defaults.
        let p = parse_failover_policy(r#"{"enabled":true,"chain":["codex-cli"]}"#);
        assert!(p.enabled);
        assert_eq!(p.threshold_pct, DEFAULT_THRESHOLD_PCT);
        assert!(p.on_transient_error);
        crate::paths::test_home::with_temp_home(|home| {
            assert_eq!(load_failover_policy(), FailoverPolicy::default());
            let written = write_failover_policy(&FailoverPolicy {
                enabled: true,
                chain: vec!["codex-cli".into(), "codex-cli".into()],
                threshold_pct: 90.0,
                on_transient_error: false,
                on_warning: true,
            })
            .unwrap();
            assert!(home.join(".cortex").join("failover.json").is_file());
            assert_eq!(written.chain, vec!["codex-cli"]);
            assert_eq!(load_failover_policy(), written);
        });
    }

    #[test]
    fn claude_snapshot_parse_and_staleness() {
        let now = 1_700_000_000_000i64;
        let fresh = now - 60_000;
        let body = |status: &str, resets_s: i64, ooc: bool| {
            format!(
                r#"{{"status":"{status}","resets_at":{resets_s},"rate_limit_type":"five_hour","out_of_credits":{ooc},"updated_ms":{fresh}}}"#
            )
        };
        let future_s = now / 1000 + 3600;
        let s = parse_claude_limit_snapshot(&body("rejected", future_s, false), now).unwrap();
        assert!(s.limit_reached);
        assert!(!s.warning);
        assert_eq!(s.window.as_deref(), Some("five_hour"));
        let s =
            parse_claude_limit_snapshot(&body("allowed_warning", future_s, false), now).unwrap();
        assert!(!s.limit_reached);
        assert!(s.warning);
        let s = parse_claude_limit_snapshot(&body("allowed", future_s, true), now).unwrap();
        assert!(s.limit_reached, "out_of_credits counts as reached");
        let s = parse_claude_limit_snapshot(&body("allowed", future_s, false), now).unwrap();
        assert_eq!(
            s,
            UsageSnapshot {
                window: Some("five_hour".into()),
                ..Default::default()
            }
        );
        // Window already reset → ignored.
        assert_eq!(
            parse_claude_limit_snapshot(&body("rejected", now / 1000 - 5, false), now),
            None
        );
        // Millisecond resets_at is tolerated.
        assert!(parse_claude_limit_snapshot(&body("rejected", now + 5_000, false), now).is_some());
        // Too old → ignored; garbage → ignored.
        let old = format!(
            r#"{{"status":"rejected","updated_ms":{}}}"#,
            now - SNAPSHOT_MAX_AGE_MS - 1
        );
        assert_eq!(parse_claude_limit_snapshot(&old, now), None);
        assert_eq!(parse_claude_limit_snapshot("nope", now), None);
        assert_eq!(
            parse_claude_limit_snapshot(r#"{"status":"rejected"}"#, now),
            None
        );
    }

    // ---- chain walk against a stub registry ----

    struct Stub {
        id: &'static str,
        available: bool,
        chat: bool,
    }

    #[async_trait]
    impl AgentAdapter for Stub {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.id.to_string(),
                label: self.id.to_string(),
                description: String::new(),
                capabilities: if self.chat {
                    vec![AgentCapability::Chat]
                } else {
                    vec![AgentCapability::CodeEdit]
                },
                available: self.available,
            }
        }
        async fn health_check(&self) -> bool {
            self.available
        }
        async fn run(
            &self,
            _req: ChatRequest,
            _tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::new();
        for (id, available, chat) in [
            ("claude-cli", true, true),
            ("codex-cli", false, true), // installed? no
            ("gemini-cli", true, true),
            ("editor-only", true, false),
            ("lmstudio", true, true),
        ] {
            r.register(Arc::new(Stub {
                id,
                available,
                chat,
            }));
        }
        r
    }

    #[test]
    fn chain_walk_skips_excluded_unavailable_and_non_chat() {
        let reg = registry();
        let p = armed(&[
            "claude-cli",
            "codex-cli",
            "editor-only",
            "gemini-cli",
            "lmstudio",
        ]);
        // Primary excluded, codex unavailable, editor-only can't chat → gemini.
        assert_eq!(
            pick_fallback_agent(&p, &reg, &["claude-cli"]).as_deref(),
            Some("gemini-cli")
        );
        // Everything usable already failed → lmstudio; then nothing.
        assert_eq!(
            pick_fallback_agent(&p, &reg, &["claude-cli", "gemini-cli"]).as_deref(),
            Some("lmstudio")
        );
        // A health poll that saw lmstudio down removes it too.
        reg.record_health("lmstudio", false);
        assert_eq!(
            pick_fallback_agent(&p, &reg, &["claude-cli", "gemini-cli"]),
            None
        );
        // Unknown ids are skipped; whitespace tolerated.
        let p = armed(&["nope", " gemini-cli "]);
        assert_eq!(
            pick_fallback_agent(&p, &reg, &[]).as_deref(),
            Some("gemini-cli")
        );
    }
}
