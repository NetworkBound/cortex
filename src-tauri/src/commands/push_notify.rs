//! Phone push notifications via ntfy or Gotify.
//!
//! Desktop toasts (`commands::notify`) only help while the user is at the
//! machine. This module pings a phone instead: when a run needs approval,
//! finishes, fails, or an account quota is nearly exhausted, Cortex POSTs to a
//! self-hosted (or public) ntfy / Gotify server. The notification's click
//! target deep-links into the mobile PWA inbox so a tap lands on the approval.
//!
//! Config lives at `~/.cortex/push.json` (no secrets). The server token is
//! kept in the encrypted key vault (`commands::keyvault`) under provider
//! `push` / label `token`, never in the JSON file.
//!
//! Egress is guarded by the webhooks SSRF check. Because a homelab ntfy on
//! the LAN or tailnet is the *normal* deployment, the config carries an
//! explicit `allow_private_host` opt-in that admits RFC1918 / 100.64/10 /
//! IPv6-ULA targets — loopback and link-local stay rejected regardless.
//!
//! Sending is always best-effort and off the caller's thread: fire points in
//! the chat loop / routines scheduler call [`fire_detached`], which spawns a
//! short-lived OS thread, rate-limits, builds the request and POSTs with a
//! 5 s timeout. Errors are logged via `tracing`, never surfaced.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PUSH_FILENAME: &str = "push.json";
const VAULT_PROVIDER: &str = "push";
const VAULT_LABEL: &str = "token";
const REQUEST_TIMEOUT_SECS: u64 = 5;
/// Body cap (chars) — ntfy truncates around 4 KB; keep pushes glanceable.
const BODY_CAP: usize = 1024;
const TITLE_CAP: usize = 200;
/// Identical (event, key) pairs are sent at most once per this window.
const DEDUPE_TTL: Duration = Duration::from_secs(10 * 60);
/// Global ceiling across all events, per rolling minute.
const MAX_PER_MINUTE: usize = 10;
/// Account-usage percentage at or above which `quota_low` fires.
const QUOTA_LOW_PCT: f64 = 90.0;

pub const EVENT_APPROVAL_NEEDED: &str = "approval_needed";
pub const EVENT_RUN_FINISHED: &str = "run_finished";
pub const EVENT_RUN_FAILED: &str = "run_failed";
pub const EVENT_QUOTA_LOW: &str = "quota_low";
pub const ALL_EVENTS: [&str; 4] = [
    EVENT_APPROVAL_NEEDED,
    EVENT_RUN_FINISHED,
    EVENT_RUN_FAILED,
    EVENT_QUOTA_LOW,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Ntfy,
    Gotify,
}

/// On-disk + over-the-bridge config. Contains no secret: the token is
/// resolved from the key vault at send time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: Provider,
    /// Server base URL, e.g. `https://ntfy.sh`, `http://100.101.1.2:8080`.
    #[serde(default)]
    pub server_url: String,
    /// ntfy topic. Ignored for Gotify (the app token selects the target).
    #[serde(default)]
    pub topic: String,
    /// Subset of [`ALL_EVENTS`] to forward.
    #[serde(default = "default_events")]
    pub events: Vec<String>,
    /// Admit RFC1918 / 100.64/10 / IPv6-ULA servers (LAN, tailnet).
    #[serde(default)]
    pub allow_private_host: bool,
    /// Base URL of the mobile PWA as the *phone* reaches it (typically the
    /// `tailscale serve` HTTPS URL). When blank we derive it from the live
    /// Tailscale MagicDNS name, and omit the click link if that's unknown.
    #[serde(default)]
    pub mobile_url: String,
}

fn default_events() -> Vec<String> {
    vec![
        EVENT_APPROVAL_NEEDED.to_string(),
        EVENT_RUN_FAILED.to_string(),
    ]
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::Ntfy,
            server_url: String::new(),
            topic: String::new(),
            events: default_events(),
            allow_private_host: false,
            mobile_url: String::new(),
        }
    }
}

/// What the settings UI sees: the config plus whether a token is stored.
#[derive(Debug, Clone, Serialize)]
pub struct PushConfigView {
    #[serde(flatten)]
    pub config: PushConfig,
    pub has_token: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// A notification before it is shaped for a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushMessage {
    pub title: String,
    pub body: String,
    /// ntfy scale 1 (min) … 5 (max). Mapped for Gotify.
    pub priority: u8,
    /// ntfy tags (emoji shortcodes). Gotify has no equivalent; dropped.
    pub tags: Vec<String>,
    pub click: Option<String>,
}

/// Provider-shaped HTTP request. Pure data so builders are unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// JSON body.
    pub body: String,
}

// ---------- config persistence ----------

fn config_path() -> Result<PathBuf, String> {
    let home = crate::paths::home_dir().ok_or_else(|| "no home directory".to_string())?;
    Ok(home.join(".cortex").join(PUSH_FILENAME))
}

/// Missing or unreadable file → defaults (feature is opt-in; absence is the
/// common case). A malformed file is logged and treated as default too.
pub fn load_config() -> PushConfig {
    let path = match config_path() {
        Ok(p) => p,
        Err(_) => return PushConfig::default(),
    };
    match fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => match serde_json::from_slice::<PushConfig>(&bytes) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("push: ignoring malformed {}: {e}", path.display());
                PushConfig::default()
            }
        },
        _ => PushConfig::default(),
    }
}

fn save_config(cfg: &PushConfig) -> Result<(), String> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("mkdir ~/.cortex: {e}"))?;
    }
    let body = serde_json::to_vec_pretty(cfg).map_err(|e| format!("encode push config: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &body).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Token from the vault; `None` when unset *or* when the keychain is
/// unavailable (the latter is logged — a push must not fail loudly).
fn load_token() -> Option<String> {
    match crate::commands::keyvault::lookup_key_sync(VAULT_PROVIDER, VAULT_LABEL) {
        Ok(t) if !t.trim().is_empty() => Some(t),
        Ok(_) => None,
        Err(e) => {
            if !e.starts_with("no key for") {
                tracing::warn!("push: token lookup failed: {e}");
            }
            None
        }
    }
}

// ---------- validation ----------

/// Egress guard: shared SSRF check, with the private-network opt-in taken
/// from the config. Never relaxes loopback / link-local.
pub fn validate_server_url(cfg: &PushConfig) -> Result<(), String> {
    let url = cfg.server_url.trim();
    if url.is_empty() {
        return Err("server URL must not be empty".into());
    }
    crate::observability::webhooks::validate_egress_url_allowing_private(
        url,
        cfg.allow_private_host,
    )
}

/// ntfy topic rule: `[-_A-Za-z0-9]{1,64}`. Rejecting anything else also
/// keeps the topic safe to splice into a URL path unescaped.
fn validate_topic(topic: &str) -> Result<(), String> {
    let t = topic.trim();
    if t.is_empty() || t.len() > 64 {
        return Err("ntfy topic must be 1–64 characters".into());
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("ntfy topic may only contain letters, digits, '-' and '_'".into());
    }
    Ok(())
}

fn validate_events(events: &[String]) -> Result<(), String> {
    for e in events {
        if !ALL_EVENTS.contains(&e.as_str()) {
            return Err(format!("unknown push event {e:?}"));
        }
    }
    Ok(())
}

/// Full config check used by `push_set_config` (when enabled) and `push_test`.
fn validate_config(cfg: &PushConfig) -> Result<(), String> {
    validate_server_url(cfg)?;
    validate_events(&cfg.events)?;
    if cfg.provider == Provider::Ntfy {
        validate_topic(&cfg.topic)?;
    }
    if !cfg.mobile_url.trim().is_empty() {
        let m = cfg.mobile_url.trim();
        if !(m.starts_with("http://") || m.starts_with("https://")) {
            return Err("mobile URL must start with http:// or https://".into());
        }
    }
    Ok(())
}

// ---------- pure builders ----------

fn clip(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Deep link for the phone. `mobile_url` (user override) wins; otherwise the
/// tailnet MagicDNS name (the `tailscale serve` HTTPS origin). `#inbox` opens
/// the PWA's approvals tab. `None` when neither is known — the notification
/// then simply has no click target rather than a dead link.
pub fn deep_link(mobile_url: &str, ts_dnsname: Option<&str>, inbox: bool) -> Option<String> {
    let base = {
        let m = mobile_url.trim().trim_end_matches('/');
        if !m.is_empty() {
            m.to_string()
        } else {
            let d = ts_dnsname?.trim().trim_end_matches('.');
            if d.is_empty() {
                return None;
            }
            format!("https://{d}")
        }
    };
    Some(if inbox {
        format!("{base}/#inbox")
    } else {
        format!("{base}/")
    })
}

/// (priority, tags) per event. Approvals and failures are "high" so the phone
/// buzzes; a finished run and a quota warning are default-priority.
pub fn event_style(event: &str) -> (u8, &'static [&'static str]) {
    match event {
        EVENT_APPROVAL_NEEDED => (4, &["bell"]),
        EVENT_RUN_FAILED => (4, &["x"]),
        EVENT_RUN_FINISHED => (3, &["white_check_mark"]),
        EVENT_QUOTA_LOW => (3, &["hourglass"]),
        _ => (3, &[]),
    }
}

/// Assemble a [`PushMessage`] for `event`, applying caps and the deep link.
pub fn build_message(cfg: &PushConfig, event: &str, title: &str, body: &str) -> PushMessage {
    let (priority, tags) = event_style(event);
    let dnsname = tailscale_dnsname();
    PushMessage {
        title: clip(title, TITLE_CAP),
        body: clip(body, BODY_CAP),
        priority,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        click: deep_link(
            &cfg.mobile_url,
            dnsname.as_deref(),
            event == EVENT_APPROVAL_NEEDED,
        ),
    }
}

fn tailscale_dnsname() -> Option<String> {
    match crate::tailscale::current_status() {
        crate::tailscale::TsStatus::Connected { dnsname, .. } if !dnsname.trim().is_empty() => {
            Some(dnsname)
        }
        _ => None,
    }
}

/// ntfy JSON publish: `POST <server>` with `{topic,title,message,...}`.
/// Chosen over the header form because JSON carries UTF-8 titles without
/// RFC 2047 encoding. Token (if any) goes in `Authorization: Bearer`.
pub fn ntfy_request(
    cfg: &PushConfig,
    token: Option<&str>,
    msg: &PushMessage,
) -> Result<PreparedRequest, String> {
    validate_topic(&cfg.topic)?;
    let server = cfg.server_url.trim().trim_end_matches('/');
    if server.is_empty() {
        return Err("server URL must not be empty".into());
    }
    let mut json = serde_json::json!({
        "topic": cfg.topic.trim(),
        "title": msg.title,
        "message": msg.body,
        "priority": msg.priority.clamp(1, 5),
    });
    if !msg.tags.is_empty() {
        json["tags"] = serde_json::Value::from(msg.tags.clone());
    }
    if let Some(click) = &msg.click {
        json["click"] = serde_json::Value::from(click.as_str());
    }
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) {
        headers.push(("Authorization".to_string(), format!("Bearer {t}")));
    }
    Ok(PreparedRequest {
        url: server.to_string(),
        headers,
        body: json.to_string(),
    })
}

/// ntfy 1..5 → Gotify 0..10 (Gotify: 0 silent, 1–3 low, 4–7 normal, 8+ high).
pub fn gotify_priority(ntfy: u8) -> u8 {
    match ntfy {
        0 | 1 => 1,
        2 => 3,
        3 => 5,
        4 => 8,
        _ => 10,
    }
}

/// Gotify: `POST <server>/message` with the app token in `X-Gotify-Key`
/// (equivalent to `?token=`, but keeps the secret out of URLs and access
/// logs). Click target rides in `extras["client::notification"].click.url`.
pub fn gotify_request(
    cfg: &PushConfig,
    token: Option<&str>,
    msg: &PushMessage,
) -> Result<PreparedRequest, String> {
    let server = cfg.server_url.trim().trim_end_matches('/');
    if server.is_empty() {
        return Err("server URL must not be empty".into());
    }
    let token = token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Gotify needs an application token".to_string())?;
    let mut json = serde_json::json!({
        "title": msg.title,
        "message": msg.body,
        "priority": gotify_priority(msg.priority),
    });
    if let Some(click) = &msg.click {
        json["extras"] = serde_json::json!({
            "client::notification": { "click": { "url": click } }
        });
    }
    Ok(PreparedRequest {
        url: format!("{server}/message"),
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Gotify-Key".to_string(), token.to_string()),
        ],
        body: json.to_string(),
    })
}

pub fn build_request(
    cfg: &PushConfig,
    token: Option<&str>,
    msg: &PushMessage,
) -> Result<PreparedRequest, String> {
    match cfg.provider {
        Provider::Ntfy => ntfy_request(cfg, token, msg),
        Provider::Gotify => gotify_request(cfg, token, msg),
    }
}

// ---------- rate limiting ----------

/// Two-layer limiter: (a) an identical `key` is admitted once per
/// `dedupe_ttl`, so a re-emitted approval for the same run doesn't buzz
/// twice; (b) at most `max_per_window` sends per `window` overall, so a
/// runaway loop can't flood the phone. Pure — the caller supplies `now`.
#[derive(Debug)]
pub struct RateLimiter {
    seen: HashMap<String, Instant>,
    recent: VecDeque<Instant>,
    dedupe_ttl: Duration,
    window: Duration,
    max_per_window: usize,
}

impl RateLimiter {
    pub fn new(dedupe_ttl: Duration, window: Duration, max_per_window: usize) -> Self {
        Self {
            seen: HashMap::new(),
            recent: VecDeque::new(),
            dedupe_ttl,
            window,
            max_per_window,
        }
    }

    /// True if a send for `key` may proceed at `now` (and records it).
    pub fn allow(&mut self, key: &str, now: Instant) -> bool {
        // Expire old entries first so the maps stay bounded.
        self.seen
            .retain(|_, t| now.saturating_duration_since(*t) < self.dedupe_ttl);
        while let Some(front) = self.recent.front() {
            if now.saturating_duration_since(*front) >= self.window {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.seen.contains_key(key) {
            return false;
        }
        if self.recent.len() >= self.max_per_window {
            return false;
        }
        self.seen.insert(key.to_string(), now);
        self.recent.push_back(now);
        true
    }
}

fn limiter() -> &'static Mutex<RateLimiter> {
    static L: once_cell::sync::Lazy<Mutex<RateLimiter>> = once_cell::sync::Lazy::new(|| {
        Mutex::new(RateLimiter::new(
            DEDUPE_TTL,
            Duration::from_secs(60),
            MAX_PER_MINUTE,
        ))
    });
    &L
}

// ---------- sending ----------

async fn send_async(req: &PreparedRequest, started: Instant) -> SendResult {
    let fail = |error: String| SendResult {
        ok: false,
        status: None,
        latency_ms: started.elapsed().as_millis() as u64,
        error: Some(error),
    };
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
    // A tailnet ntfy reached through the embedded Tailscale sidecar needs its
    // SOCKS proxy; a no-op when Tailscale is off or the system node is used.
    let builder = crate::tailscale::maybe_tailscale_proxy(builder);
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return fail(format!("client: {e}")),
    };
    let mut r = client.post(&req.url).body(req.body.clone());
    for (k, v) in &req.headers {
        r = r.header(k.as_str(), v.as_str());
    }
    match r.send().await {
        Ok(resp) => {
            let status = resp.status();
            SendResult {
                ok: status.is_success(),
                status: Some(status.as_u16()),
                latency_ms: started.elapsed().as_millis() as u64,
                error: if status.is_success() {
                    None
                } else {
                    Some(format!("http {}", status.as_u16()))
                },
            }
        }
        Err(e) => fail(e.to_string()),
    }
}

/// Resolve config + token, guard the URL, build and send. Shared by the test
/// command and the detached fire path. Returns `Ok(None)` when nothing was
/// sent because the feature is off or `event` isn't subscribed.
async fn send_event(
    cfg: &PushConfig,
    event: &str,
    title: &str,
    body: &str,
    force: bool,
) -> Result<Option<SendResult>, String> {
    if !force && (!cfg.enabled || !cfg.events.iter().any(|e| e == event)) {
        return Ok(None);
    }
    validate_server_url(cfg)?;
    let token = load_token();
    let msg = build_message(cfg, event, title, body);
    let req = build_request(cfg, token.as_deref(), &msg)?;
    Ok(Some(send_async(&req, Instant::now()).await))
}

/// Non-blocking, best-effort push. Safe to call from any thread, inside or
/// outside a tokio runtime (the send runs on its own OS thread with a
/// current-thread runtime — same reasoning as `webhooks::post_blocking`).
/// `dedupe_key` should identify the occurrence (e.g. `approval:<run_id>`).
pub fn fire_detached(event: &'static str, dedupe_key: String, title: String, body: String) {
    let cfg = load_config();
    if !cfg.enabled || !cfg.events.iter().any(|e| e == event) {
        return;
    }
    {
        let mut l = limiter().lock().unwrap_or_else(|e| e.into_inner());
        if !l.allow(&format!("{event}:{dedupe_key}"), Instant::now()) {
            tracing::debug!("push: rate-limited {event} ({dedupe_key})");
            return;
        }
    }
    let spawned = std::thread::Builder::new()
        .name("cortex-push-send".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("push: runtime: {e}");
                    return;
                }
            };
            match rt.block_on(send_event(&cfg, event, &title, &body, false)) {
                Ok(Some(r)) if !r.ok => {
                    tracing::warn!(
                        "push: {event} failed: status={:?} err={:?}",
                        r.status,
                        r.error
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("push: {event} not sent: {e}"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("push: spawn send thread: {e}");
    }
}

// ---------- fire-point helpers (keep call sites one-liners) ----------

/// An agent run is waiting on the user. Deep-links to the mobile inbox.
pub fn notify_approval_needed(run_id: &str, agent_id: &str, tool: Option<&str>) {
    let body = match tool {
        Some(t) if !t.is_empty() => format!("{agent_id} wants to run {t}"),
        _ => format!("{agent_id} is waiting for your approval"),
    };
    fire_detached(
        EVENT_APPROVAL_NEEDED,
        format!("approval:{run_id}"),
        "Cortex: approval needed".to_string(),
        body,
    );
}

/// An agent run ended. `error` is `Some` when it failed.
pub fn notify_run_outcome(run_id: &str, agent_id: &str, error: Option<&str>) {
    match error {
        Some(e) => fire_detached(
            EVENT_RUN_FAILED,
            format!("run:{run_id}"),
            format!("Cortex: {agent_id} failed"),
            e.to_string(),
        ),
        None => fire_detached(
            EVENT_RUN_FINISHED,
            format!("run:{run_id}"),
            format!("Cortex: {agent_id} finished"),
            "Run completed".to_string(),
        ),
    }
}

/// A scheduled routine run failed.
pub fn notify_routine_failed(run_id: &str, routine_name: &str, error: &str) {
    fire_detached(
        EVENT_RUN_FAILED,
        format!("routine:{run_id}"),
        format!("Cortex: routine \u{201c}{routine_name}\u{201d} failed"),
        error.to_string(),
    );
}

/// `(dedupe_key, title, body)` for every usage window at/over the threshold.
/// The key embeds the window's reset marker so each exhausted window pings
/// once, then goes quiet until it resets and fills up again.
pub fn quota_alerts(
    usage: &crate::commands::account_usage::AccountUsage,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Some(c) = &usage.claude {
        if c.five_hour_pct >= QUOTA_LOW_PCT {
            out.push((
                format!(
                    "claude:5h:{}",
                    c.five_hour_resets_at.clone().unwrap_or_default()
                ),
                "Cortex: Claude 5-hour quota almost used".to_string(),
                format!("{:.0}% of the 5-hour window used", c.five_hour_pct),
            ));
        }
        if c.seven_day_pct >= QUOTA_LOW_PCT {
            out.push((
                format!(
                    "claude:7d:{}",
                    c.seven_day_resets_at.clone().unwrap_or_default()
                ),
                "Cortex: Claude weekly quota almost used".to_string(),
                format!("{:.0}% of the 7-day window used", c.seven_day_pct),
            ));
        }
    }
    if let Some(g) = &usage.chatgpt {
        if g.limit_reached || g.primary_used_pct >= QUOTA_LOW_PCT {
            out.push((
                format!("chatgpt:primary:{}", g.primary_reset_at),
                "Cortex: ChatGPT quota almost used".to_string(),
                if g.limit_reached {
                    "Rate limit reached".to_string()
                } else {
                    format!("{:.0}% of the primary window used", g.primary_used_pct)
                },
            ));
        }
    }
    out
}

/// Fire `quota_low` for each exhausted window (deduped by the limiter).
pub fn notify_quota(usage: &crate::commands::account_usage::AccountUsage) {
    for (key, title, body) in quota_alerts(usage) {
        fire_detached(EVENT_QUOTA_LOW, key, title, body);
    }
}

// ---------- tauri commands ----------

#[tauri::command]
pub async fn push_get_config() -> Result<PushConfigView, String> {
    let config = load_config();
    let has_token = load_token().is_some();
    Ok(PushConfigView { config, has_token })
}

/// Persist the config. `token`: `Some(non-empty)` stores a new token,
/// `clear_token` removes the stored one; otherwise the token is kept as-is
/// (the UI never receives it back). The URL is validated against the SSRF
/// guard whenever it is non-empty, so a bad host is caught at save time.
#[tauri::command]
pub async fn push_set_config(
    config: PushConfig,
    token: Option<String>,
    clear_token: Option<bool>,
) -> Result<(), String> {
    let mut cfg = config;
    cfg.server_url = cfg.server_url.trim().to_string();
    cfg.topic = cfg.topic.trim().to_string();
    cfg.mobile_url = cfg.mobile_url.trim().to_string();
    validate_events(&cfg.events)?;
    if cfg.enabled {
        validate_config(&cfg)?;
    } else if !cfg.server_url.is_empty() {
        validate_server_url(&cfg)?;
    }
    save_config(&cfg)?;
    if clear_token.unwrap_or(false) {
        // Removing a token that isn't there is not an error for the UI.
        let _ = crate::commands::keyvault::vault_remove(
            VAULT_PROVIDER.to_string(),
            VAULT_LABEL.to_string(),
        )
        .await;
    } else if let Some(t) = token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        crate::commands::keyvault::vault_set(
            VAULT_PROVIDER.to_string(),
            VAULT_LABEL.to_string(),
            t,
        )
        .await?;
    }
    Ok(())
}

/// Send a synthetic notification with the *saved* config (save first).
/// Bypasses the enabled/event filters and the rate limiter so the user can
/// verify connectivity, but not the SSRF guard.
#[tauri::command]
pub async fn push_test() -> Result<SendResult, String> {
    let cfg = load_config();
    validate_config(&cfg)?;
    let body = format!(
        "Test notification from Cortex ({})",
        chrono::Local::now().format("%H:%M:%S")
    );
    match send_event(
        &cfg,
        EVENT_APPROVAL_NEEDED,
        "Cortex: test push",
        &body,
        true,
    )
    .await?
    {
        Some(r) => Ok(r),
        None => Err("nothing sent".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(provider: Provider) -> PushConfig {
        PushConfig {
            enabled: true,
            provider,
            server_url: "https://push.example.com/".into(),
            topic: "cortex_alerts".into(),
            events: default_events(),
            allow_private_host: false,
            mobile_url: String::new(),
        }
    }

    fn msg() -> PushMessage {
        PushMessage {
            title: "Cortex: approval needed".into(),
            body: "claude wants to run bash".into(),
            priority: 4,
            tags: vec!["bell".into()],
            click: Some("https://box.tail1234.ts.net/#inbox".into()),
        }
    }

    #[test]
    fn ntfy_json_publish_shape() {
        let req = ntfy_request(&cfg(Provider::Ntfy), Some("tk-123"), &msg()).unwrap();
        assert_eq!(req.url, "https://push.example.com");
        let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(v["topic"], "cortex_alerts");
        assert_eq!(v["title"], "Cortex: approval needed");
        assert_eq!(v["message"], "claude wants to run bash");
        assert_eq!(v["priority"], 4);
        assert_eq!(v["tags"][0], "bell");
        assert_eq!(v["click"], "https://box.tail1234.ts.net/#inbox");
        assert!(req
            .headers
            .contains(&("Authorization".to_string(), "Bearer tk-123".to_string())));
        assert!(req
            .headers
            .contains(&("Content-Type".to_string(), "application/json".to_string())));
    }

    #[test]
    fn ntfy_without_token_or_click_omits_them() {
        let mut m = msg();
        m.click = None;
        m.tags.clear();
        let req = ntfy_request(&cfg(Provider::Ntfy), None, &m).unwrap();
        let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert!(v.get("click").is_none());
        assert!(v.get("tags").is_none());
        assert!(!req.headers.iter().any(|(k, _)| k == "Authorization"));
    }

    #[test]
    fn ntfy_rejects_bad_topic() {
        let mut c = cfg(Provider::Ntfy);
        c.topic = "has space".into();
        assert!(ntfy_request(&c, None, &msg()).is_err());
        c.topic = "../etc".into();
        assert!(ntfy_request(&c, None, &msg()).is_err());
        c.topic = String::new();
        assert!(ntfy_request(&c, None, &msg()).is_err());
        c.topic = "ok-topic_1".into();
        assert!(ntfy_request(&c, None, &msg()).is_ok());
    }

    #[test]
    fn gotify_message_shape() {
        let req = gotify_request(&cfg(Provider::Gotify), Some("Axyz"), &msg()).unwrap();
        assert_eq!(req.url, "https://push.example.com/message");
        assert!(
            !req.url.contains("Axyz"),
            "token must not leak into the URL"
        );
        assert!(req
            .headers
            .contains(&("X-Gotify-Key".to_string(), "Axyz".to_string())));
        let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(v["title"], "Cortex: approval needed");
        assert_eq!(v["message"], "claude wants to run bash");
        assert_eq!(v["priority"], 8);
        assert_eq!(
            v["extras"]["client::notification"]["click"]["url"],
            "https://box.tail1234.ts.net/#inbox"
        );
    }

    #[test]
    fn gotify_requires_token_and_omits_extras_without_click() {
        assert!(gotify_request(&cfg(Provider::Gotify), None, &msg()).is_err());
        assert!(gotify_request(&cfg(Provider::Gotify), Some("  "), &msg()).is_err());
        let mut m = msg();
        m.click = None;
        let req = gotify_request(&cfg(Provider::Gotify), Some("t"), &m).unwrap();
        let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert!(v.get("extras").is_none());
    }

    #[test]
    fn priority_mapping_and_event_styles() {
        assert_eq!(gotify_priority(1), 1);
        assert_eq!(gotify_priority(3), 5);
        assert_eq!(gotify_priority(4), 8);
        assert_eq!(gotify_priority(5), 10);
        assert_eq!(event_style(EVENT_APPROVAL_NEEDED).0, 4);
        assert_eq!(event_style(EVENT_RUN_FAILED).0, 4);
        assert_eq!(event_style(EVENT_RUN_FINISHED).0, 3);
        assert!(event_style("bogus").1.is_empty());
    }

    #[test]
    fn deep_link_prefers_override_then_tailnet_else_none() {
        assert_eq!(
            deep_link("https://cortex.example/", None, true).as_deref(),
            Some("https://cortex.example/#inbox")
        );
        assert_eq!(
            deep_link("", Some("box.tail1234.ts.net."), true).as_deref(),
            Some("https://box.tail1234.ts.net/#inbox")
        );
        assert_eq!(
            deep_link("", Some("box.tail1234.ts.net"), false).as_deref(),
            Some("https://box.tail1234.ts.net/")
        );
        assert_eq!(deep_link("  ", None, true), None);
        assert_eq!(deep_link("", Some(""), true), None);
    }

    #[test]
    fn server_url_guard_honours_allow_private_host() {
        let mut c = cfg(Provider::Ntfy);
        c.server_url = "http://192.168.1.10:8080".into();
        assert!(validate_server_url(&c).is_err());
        c.allow_private_host = true;
        assert!(validate_server_url(&c).is_ok());
        c.server_url = "http://100.100.1.2".into();
        assert!(validate_server_url(&c).is_ok());
        // Loopback + metadata stay rejected even with the opt-in.
        c.server_url = "http://127.0.0.1:2586".into();
        assert!(validate_server_url(&c).is_err());
        c.server_url = "http://169.254.169.254/".into();
        assert!(validate_server_url(&c).is_err());
        c.server_url = String::new();
        assert!(validate_server_url(&c).is_err());
    }

    #[test]
    fn validate_config_checks_events_and_mobile_url() {
        let mut c = cfg(Provider::Ntfy);
        c.server_url = "https://8.8.8.8".into();
        assert!(validate_config(&c).is_ok());
        c.events.push("made_up".into());
        assert!(validate_config(&c).is_err());
        c.events = default_events();
        c.mobile_url = "box.example".into();
        assert!(validate_config(&c).is_err());
        c.mobile_url = "https://box.example".into();
        assert!(validate_config(&c).is_ok());
        // Gotify doesn't need a topic.
        c.provider = Provider::Gotify;
        c.topic = String::new();
        assert!(validate_config(&c).is_ok());
    }

    #[test]
    fn rate_limiter_dedupes_and_caps_per_window() {
        let mut rl = RateLimiter::new(Duration::from_secs(600), Duration::from_secs(60), 3);
        let t0 = Instant::now();
        assert!(rl.allow("approval:r1", t0));
        // Same key again inside the TTL: suppressed.
        assert!(!rl.allow("approval:r1", t0 + Duration::from_secs(1)));
        assert!(rl.allow("approval:r2", t0 + Duration::from_secs(2)));
        assert!(rl.allow("run:r3", t0 + Duration::from_secs(3)));
        // Window cap (3/min) reached: a fresh key is refused...
        assert!(!rl.allow("run:r4", t0 + Duration::from_secs(4)));
        // ...until the window slides past the oldest send.
        assert!(rl.allow("run:r4", t0 + Duration::from_secs(61)));
        // Dedupe TTL expiry lets the original key through again.
        assert!(rl.allow("approval:r1", t0 + Duration::from_secs(700)));
    }

    #[test]
    fn config_roundtrip_and_defaults() {
        let parsed: PushConfig = serde_json::from_str(r#"{"provider":"gotify"}"#).unwrap();
        assert_eq!(parsed.provider, Provider::Gotify);
        assert!(!parsed.enabled);
        assert!(!parsed.allow_private_host);
        assert_eq!(parsed.events, default_events());
        let back = serde_json::to_value(&cfg(Provider::Ntfy)).unwrap();
        assert_eq!(back["provider"], "ntfy");
        // The view carries no token, only a flag.
        let view = PushConfigView {
            config: cfg(Provider::Ntfy),
            has_token: true,
        };
        let s = serde_json::to_string(&view).unwrap();
        assert!(s.contains("\"has_token\":true"));
        assert!(s.contains("\"topic\":\"cortex_alerts\""));
    }

    #[test]
    fn load_config_defaults_when_missing_or_malformed() {
        crate::paths::test_home::with_temp_home(|home| {
            assert!(!load_config().enabled);
            let dir = home.join(".cortex");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(PUSH_FILENAME), b"{not json").unwrap();
            assert!(!load_config().enabled);
            let mut c = cfg(Provider::Ntfy);
            c.allow_private_host = true;
            save_config(&c).unwrap();
            let loaded = load_config();
            assert!(loaded.enabled && loaded.allow_private_host);
            assert_eq!(loaded.topic, "cortex_alerts");
        });
    }

    #[test]
    fn clip_caps_on_char_boundary_with_ellipsis() {
        assert_eq!(clip("short", 10), "short");
        let long = "héllo wörld 🌍 and more".repeat(100);
        let out = clip(&long, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn quota_alerts_fire_only_over_threshold_and_key_on_reset() {
        use crate::commands::account_usage::{AccountUsage, ChatgptUsage, ClaudeUsage};
        let claude = ClaudeUsage {
            five_hour_pct: 95.0,
            five_hour_resets_at: Some("2026-09-28T12:00:00Z".into()),
            seven_day_pct: 40.0,
            seven_day_resets_at: None,
            sonnet_pct: None,
            extra_monthly_limit: None,
            extra_used_credits: None,
            currency: None,
        };
        let chatgpt = ChatgptUsage {
            plan_type: "plus".into(),
            primary_used_pct: 10.0,
            primary_reset_at: 1,
            secondary_used_pct: 0.0,
            secondary_reset_at: 0,
            limit_reached: true,
            credits_balance: None,
        };
        let alerts = quota_alerts(&AccountUsage {
            claude: Some(claude),
            chatgpt: Some(chatgpt),
        });
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0].0, "claude:5h:2026-09-28T12:00:00Z");
        assert!(alerts[0].2.contains("95%"));
        assert_eq!(alerts[1].0, "chatgpt:primary:1");
        assert_eq!(alerts[1].2, "Rate limit reached");
        assert!(quota_alerts(&AccountUsage {
            claude: None,
            chatgpt: None,
        })
        .is_empty());
    }
}
