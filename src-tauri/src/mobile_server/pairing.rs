//! Phone pairing + device registry + bearer gate for the mobile API (v2).
//!
//! Flow (see `mobile-contract.md`):
//!
//! 1. Desktop Settings calls the `ts_mobile_pairing` Tauri command, which
//!    [`mint_code`]s a one-time 6-digit code valid for [`CODE_TTL_MS`] and
//!    renders it into the QR (`cortex://pair?url=…&code=…`).
//! 2. The phone POSTs `/api/v2/pair { code, device_name }`. [`redeem_code`]
//!    consumes the code (single use) and [`register_device`] mints a 32-byte
//!    random bearer token, storing only its SHA-256 in
//!    `~/.cortex/mobile-devices.json` — the plaintext token is returned once
//!    and never persisted.
//! 3. Every later request from a non-local peer carries
//!    `Authorization: Bearer <token>` (or `?token=` on the WebSocket) and is
//!    checked by [`check_access`]: constant-time compare of the hash against
//!    every device (the compare is per-device so the comparison count leaks
//!    only the number of paired devices, which is not secret).
//!
//! "Local" means the TCP peer is loopback AND the request was not forwarded
//! (`tailscale serve` and reverse proxies connect from loopback but stamp
//! `X-Forwarded-For` / `Forwarded` / `Tailscale-User-Login`). Loopback
//! callers without those headers stay unauthenticated, exactly like the
//! legacy `/api/*` routes. `CORTEX_E2E_FORCE_AUTH=1` makes loopback require
//! auth too so the E2E smoke can exercise the gate on 127.0.0.1.
//!
//! The code + device stores are process-global (a `static`, and a file) rather
//! than living on `MobileState` because the Tauri commands that mint codes and
//! list/revoke devices run outside the axum handlers.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use super::mcp::ct_eq;

/// A pairing code is valid for 10 minutes.
pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;

/// Ceiling on unredeemed codes kept in memory (each new mint evicts the
/// oldest beyond this), so a Settings panel re-rendering the QR can't grow
/// the list without bound.
const MAX_PENDING_CODES: usize = 8;

/// Random bytes in a device token (hex-encoded → 64 chars).
const TOKEN_BYTES: usize = 32;

/// `last_seen_ms` is rewritten at most this often per device.
const LAST_SEEN_WRITE_INTERVAL_MS: i64 = 60_000;

#[derive(Debug, Clone)]
struct PendingCode {
    code: String,
    expires_ms: i64,
}

static CODES: Lazy<Mutex<Vec<PendingCode>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Serialises every read-modify-write of `mobile-devices.json`.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// `device id → last_seen_ms we last wrote`, for the write throttle.
static LAST_SEEN_WRITTEN: Lazy<Mutex<std::collections::HashMap<String, i64>>> =
    Lazy::new(|| Mutex::new(std::collections::HashMap::new()));

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ───────────────────────────────────────────────────────────────────────────
// Pairing codes
// ───────────────────────────────────────────────────────────────────────────

/// A freshly minted pairing code and when it expires (unix ms).
#[derive(Debug, Clone, Serialize)]
pub struct MintedCode {
    pub code: String,
    pub expires_ms: i64,
}

/// Six random decimal digits, uniformly distributed (rejection sampling over
/// the OS RNG so no digit pattern is favoured).
fn random_code() -> String {
    use aes_gcm::aead::rand_core::RngCore;
    loop {
        let n = aes_gcm::aead::OsRng.next_u32();
        // 4_294_000_000 is the largest multiple of 1_000_000 below u32::MAX.
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

/// Mint a new one-time code valid for [`CODE_TTL_MS`].
pub fn mint_code() -> MintedCode {
    mint_code_at(now_ms())
}

fn mint_code_at(now: i64) -> MintedCode {
    let code = random_code();
    let expires_ms = now + CODE_TTL_MS;
    let mut codes = CODES.lock().unwrap_or_else(|e| e.into_inner());
    codes.retain(|c| c.expires_ms > now);
    codes.push(PendingCode {
        code: code.clone(),
        expires_ms,
    });
    while codes.len() > MAX_PENDING_CODES {
        codes.remove(0);
    }
    MintedCode { code, expires_ms }
}

/// The E2E preset code: accepted (repeatedly, never consumed) only while
/// `CORTEX_E2E=1` AND `CORTEX_E2E_PAIR_CODE` is set. Outside E2E the env var
/// is ignored, so it can never become a backdoor in a normal session.
fn e2e_preset_code() -> Option<String> {
    if !crate::commands::e2e::e2e_enabled() {
        return None;
    }
    std::env::var("CORTEX_E2E_PAIR_CODE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Redeem `code`: true exactly once per minted code (and always for the E2E
/// preset). Expired codes are purged on the way.
pub fn redeem_code(code: &str) -> bool {
    redeem_code_at(code, now_ms())
}

fn redeem_code_at(code: &str, now: i64) -> bool {
    let code = code.trim();
    if code.is_empty() {
        return false;
    }
    if let Some(preset) = e2e_preset_code() {
        if ct_eq(preset.as_bytes(), code.as_bytes()) {
            return true;
        }
    }
    let mut codes = CODES.lock().unwrap_or_else(|e| e.into_inner());
    codes.retain(|c| c.expires_ms > now);
    let idx = codes
        .iter()
        .position(|c| ct_eq(c.code.as_bytes(), code.as_bytes()));
    match idx {
        Some(i) => {
            codes.remove(i);
            true
        }
        None => false,
    }
}

/// Drop every pending code (Settings "cancel pairing", tests).
pub fn clear_codes() {
    CODES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

// ───────────────────────────────────────────────────────────────────────────
// Device registry (~/.cortex/mobile-devices.json)
// ───────────────────────────────────────────────────────────────────────────

/// One paired device. Only the SHA-256 of its bearer token is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub token_sha256: String,
    #[serde(default)]
    pub created_ms: i64,
    #[serde(default)]
    pub last_seen_ms: i64,
}

/// A Web Push subscription registered by a paired device (RFC 8030 endpoint
/// + RFC 8291 client keys). Lives in the same file so revoking a device also
/// drops its subscriptions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushSubscription {
    pub id: String,
    pub device_id: String,
    pub endpoint: String,
    /// Base64url (unpadded) uncompressed P-256 public key (`keys.p256dh`).
    pub p256dh: String,
    /// Base64url (unpadded) 16-byte auth secret (`keys.auth`).
    pub auth: String,
    #[serde(default)]
    pub created_ms: i64,
}

/// What the API / Settings UI sees: never the hash.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub created_ms: i64,
    pub last_seen_ms: i64,
    /// Number of Web Push subscriptions this device registered.
    pub push_subscriptions: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(default)]
    devices: Vec<DeviceRecord>,
    #[serde(default)]
    push_subscriptions: Vec<PushSubscription>,
}

/// `~/.cortex/mobile-devices.json`.
pub fn devices_path() -> Option<PathBuf> {
    crate::paths::cortex_dir().map(|d| d.join("mobile-devices.json"))
}

/// Load the registry; a missing/corrupt file reads as empty (no device can
/// authenticate), never as "skip auth".
fn load_file() -> DeviceFile {
    let Some(path) = devices_path() else {
        return DeviceFile::default();
    };
    std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Persist via sibling temp file + rename (same pattern as `mcp.rs`).
fn save_file(f: &DeviceFile) -> Result<(), String> {
    let path = devices_path().ok_or_else(|| "no home dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(f).map_err(|e| format!("serialize failed: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    Ok(())
}

/// Lowercase hex SHA-256 of `token`.
pub fn hash_token(token: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn view_of(d: &DeviceRecord, subs: &[PushSubscription]) -> DeviceView {
    DeviceView {
        id: d.id.clone(),
        name: d.name.clone(),
        created_ms: d.created_ms,
        last_seen_ms: d.last_seen_ms,
        push_subscriptions: subs.iter().filter(|s| s.device_id == d.id).count(),
    }
}

/// Trim + cap a caller-supplied device name; empty → a generic label.
fn clean_name(name: &str) -> String {
    let n: String = name.trim().chars().take(64).collect();
    if n.is_empty() {
        "Mobile device".to_string()
    } else {
        n
    }
}

/// Register a new device and return its view plus the ONE-TIME plaintext
/// token.
pub fn register_device(name: &str) -> Result<(DeviceView, String), String> {
    let token = super::mcp::generate_token();
    debug_assert_eq!(token.len(), TOKEN_BYTES * 2);
    let record = DeviceRecord {
        id: format!("dev-{}", ulid::Ulid::new().to_string().to_lowercase()),
        name: clean_name(name),
        token_sha256: hash_token(&token),
        created_ms: now_ms(),
        last_seen_ms: now_ms(),
    };
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    f.devices.push(record.clone());
    save_file(&f)?;
    Ok((view_of(&record, &f.push_subscriptions), token))
}

/// Every paired device, newest first.
pub fn list_devices() -> Vec<DeviceView> {
    let f = load_file();
    let mut out: Vec<DeviceView> = f
        .devices
        .iter()
        .map(|d| view_of(d, &f.push_subscriptions))
        .collect();
    out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
    out
}

/// Revoke a device (and its push subscriptions). `Ok(false)` when unknown.
pub fn revoke_device(id: &str) -> Result<bool, String> {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    let before = f.devices.len();
    f.devices.retain(|d| d.id != id);
    if f.devices.len() == before {
        return Ok(false);
    }
    f.push_subscriptions.retain(|s| s.device_id != id);
    save_file(&f)?;
    Ok(true)
}

/// Resolve a presented bearer token to its device. Constant-time per device.
pub fn authenticate(token: &str) -> Option<DeviceView> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let presented = hash_token(token);
    let f = load_file();
    let hit = f
        .devices
        .iter()
        .find(|d| ct_eq(d.token_sha256.as_bytes(), presented.as_bytes()))?;
    let view = view_of(hit, &f.push_subscriptions);
    drop(f);
    touch_last_seen(&view.id);
    Some(view)
}

/// Bump `last_seen_ms`, throttled to one write per minute per device.
fn touch_last_seen(id: &str) {
    let now = now_ms();
    {
        let mut m = LAST_SEEN_WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(id)
            .is_some_and(|last| now - *last < LAST_SEEN_WRITE_INTERVAL_MS)
        {
            return;
        }
        m.insert(id.to_string(), now);
    }
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    if let Some(d) = f.devices.iter_mut().find(|d| d.id == id) {
        d.last_seen_ms = now;
        if let Err(e) = save_file(&f) {
            tracing::debug!("mobile devices: last_seen write failed: {e}");
        }
    }
}

// ── push subscriptions ────────────────────────────────────────────────────

/// Add (or replace, by endpoint) a push subscription for `device_id`.
pub fn add_push_subscription(
    device_id: &str,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> Result<PushSubscription, String> {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    if !f.devices.iter().any(|d| d.id == device_id) {
        return Err(format!("unknown device: {device_id}"));
    }
    // One subscription per endpoint: a re-subscribe from the same browser
    // replaces the old keys instead of piling up duplicates.
    f.push_subscriptions.retain(|s| s.endpoint != endpoint);
    let sub = PushSubscription {
        id: format!("sub-{}", ulid::Ulid::new().to_string().to_lowercase()),
        device_id: device_id.to_string(),
        endpoint: endpoint.to_string(),
        p256dh: p256dh.to_string(),
        auth: auth.to_string(),
        created_ms: now_ms(),
    };
    f.push_subscriptions.push(sub.clone());
    save_file(&f)?;
    Ok(sub)
}

/// Remove a subscription by id. When `device_id` is given, only that device's
/// own subscription can be removed (a phone can't unsubscribe another phone).
pub fn remove_push_subscription(id: &str, device_id: Option<&str>) -> Result<bool, String> {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    let before = f.push_subscriptions.len();
    f.push_subscriptions
        .retain(|s| !(s.id == id && device_id.map_or(true, |d| s.device_id == d)));
    if f.push_subscriptions.len() == before {
        return Ok(false);
    }
    save_file(&f)?;
    Ok(true)
}

/// Remove a subscription by endpoint (push service answered 404/410).
pub fn remove_push_subscription_by_endpoint(endpoint: &str) {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = load_file();
    let before = f.push_subscriptions.len();
    f.push_subscriptions.retain(|s| s.endpoint != endpoint);
    if f.push_subscriptions.len() != before {
        if let Err(e) = save_file(&f) {
            tracing::warn!("mobile devices: subscription purge failed: {e}");
        }
    }
}

/// Every stored subscription (all devices).
pub fn list_push_subscriptions() -> Vec<PushSubscription> {
    load_file().push_subscriptions
}

/// Subscriptions for one device.
pub fn push_subscriptions_for(device_id: &str) -> Vec<PushSubscription> {
    load_file()
        .push_subscriptions
        .into_iter()
        .filter(|s| s.device_id == device_id)
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Access gate
// ───────────────────────────────────────────────────────────────────────────

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Loopback, unforwarded (the desktop webview / local dev / the PWA
    /// served on 127.0.0.1). No device identity.
    Local,
    /// A paired device presenting a valid bearer token.
    Device(DeviceView),
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// No token at all.
    Missing,
    /// A token was presented but matches no paired device.
    Invalid,
}

impl Denied {
    pub fn message(self) -> &'static str {
        match self {
            Denied::Missing => "missing bearer token: pair this device first",
            Denied::Invalid => "invalid or revoked bearer token",
        }
    }
}

/// `CORTEX_E2E_FORCE_AUTH=1` — loopback requires a bearer too (test only).
pub fn force_auth_env() -> bool {
    std::env::var("CORTEX_E2E_FORCE_AUTH")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

/// Is a request from `peer` (the TCP peer, `None` when the listener didn't
/// record one) that was (`forwarded`) or wasn't stamped by a proxy a local,
/// trusted caller? `force_auth` (the E2E switch) says no regardless.
pub fn is_local_peer(peer: Option<IpAddr>, forwarded: bool, force_auth: bool) -> bool {
    if force_auth || forwarded {
        return false;
    }
    match peer {
        Some(ip) => ip.is_loopback(),
        // Fail closed: an unknown peer is treated as remote.
        None => false,
    }
}

/// Extract the token from an `Authorization: Bearer <token>` header value.
pub fn bearer_token(auth: &str) -> Option<&str> {
    let auth = auth.trim();
    let (scheme, rest) = auth.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let tok = rest.trim();
    (!tok.is_empty()).then_some(tok)
}

/// The full decision. `authorization` is the raw header value, `query_token`
/// the `?token=` value (WebSocket clients can't set headers from a browser).
/// Pure apart from the device-file read inside [`authenticate`].
pub fn check_access(
    peer: Option<IpAddr>,
    forwarded: bool,
    force_auth: bool,
    authorization: Option<&str>,
    query_token: Option<&str>,
) -> Result<Access, Denied> {
    if is_local_peer(peer, forwarded, force_auth) {
        return Ok(Access::Local);
    }
    let presented = authorization
        .and_then(bearer_token)
        .or(query_token.map(str::trim).filter(|t| !t.is_empty()));
    let Some(presented) = presented else {
        return Err(Denied::Missing);
    };
    match authenticate(presented) {
        Some(dev) => Ok(Access::Device(dev)),
        None => Err(Denied::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_home::with_temp_home;

    #[test]
    fn codes_are_six_digits_single_use_and_expire() {
        clear_codes();
        let now = 1_000_000;
        let c = mint_code_at(now);
        assert_eq!(c.code.len(), 6);
        assert!(c.code.chars().all(|ch| ch.is_ascii_digit()));
        assert_eq!(c.expires_ms, now + CODE_TTL_MS);
        // Wrong code → no.
        assert!(!redeem_code_at("000000x", now));
        // Right code → once.
        assert!(redeem_code_at(&c.code, now + 1));
        assert!(!redeem_code_at(&c.code, now + 2));
        // Expired code → no.
        let c2 = mint_code_at(now);
        assert!(!redeem_code_at(&c2.code, now + CODE_TTL_MS + 1));
        // Whitespace is tolerated, empty is not.
        let c3 = mint_code_at(now);
        assert!(!redeem_code_at("   ", now));
        assert!(redeem_code_at(&format!(" {} ", c3.code), now));
        clear_codes();
    }

    #[test]
    fn pending_codes_are_capped() {
        clear_codes();
        for _ in 0..(MAX_PENDING_CODES + 5) {
            mint_code_at(5);
        }
        assert_eq!(CODES.lock().unwrap().len(), MAX_PENDING_CODES);
        clear_codes();
    }

    #[test]
    fn hash_is_sha256_hex() {
        // SHA-256("abc")
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer   abc  "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None);
        assert_eq!(bearer_token("Bearer   "), None);
    }

    #[test]
    fn local_peer_rules() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let lo6: IpAddr = "::1".parse().unwrap();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let ts: IpAddr = "100.101.1.2".parse().unwrap();
        assert!(is_local_peer(Some(lo), false, false));
        assert!(is_local_peer(Some(lo6), false, false));
        assert!(!is_local_peer(Some(lan), false, false));
        assert!(!is_local_peer(Some(ts), false, false));
        // Proxied through tailscale serve: loopback but forwarded → remote.
        assert!(!is_local_peer(Some(lo), true, false));
        // E2E force switch.
        assert!(!is_local_peer(Some(lo), false, true));
        // Unknown peer fails closed.
        assert!(!is_local_peer(None, false, false));
    }

    #[test]
    fn device_registry_round_trip_and_auth_matrix() {
        with_temp_home(|_| {
            assert!(list_devices().is_empty());
            let (dev, token) = register_device("  Connor's iPhone ").unwrap();
            assert_eq!(dev.name, "Connor's iPhone");
            assert_eq!(token.len(), 64);
            // Stored hashed, never plaintext.
            let raw = std::fs::read_to_string(devices_path().unwrap()).unwrap();
            assert!(!raw.contains(&token));
            assert!(raw.contains(&hash_token(&token)));

            let lo: IpAddr = "127.0.0.1".parse().unwrap();
            let lan: IpAddr = "10.0.0.7".parse().unwrap();
            // Loopback, unforwarded: open.
            assert_eq!(
                check_access(Some(lo), false, false, None, None),
                Ok(Access::Local)
            );
            // LAN without a token: 401 missing.
            assert_eq!(
                check_access(Some(lan), false, false, None, None),
                Err(Denied::Missing)
            );
            // LAN with a bogus token: 401 invalid.
            assert_eq!(
                check_access(Some(lan), false, false, Some("Bearer nope"), None),
                Err(Denied::Invalid)
            );
            // LAN with the real token (header or query): the device.
            let hdr = format!("Bearer {token}");
            match check_access(Some(lan), false, false, Some(&hdr), None) {
                Ok(Access::Device(d)) => assert_eq!(d.id, dev.id),
                other => panic!("expected device access, got {other:?}"),
            }
            match check_access(Some(lan), false, false, None, Some(&token)) {
                Ok(Access::Device(d)) => assert_eq!(d.id, dev.id),
                other => panic!("expected device access, got {other:?}"),
            }
            // Forwarded loopback (tailscale serve) needs the token too.
            assert_eq!(
                check_access(Some(lo), true, false, None, None),
                Err(Denied::Missing)
            );
            // Forced auth on loopback (E2E) needs it as well.
            assert_eq!(
                check_access(Some(lo), false, true, None, None),
                Err(Denied::Missing)
            );
            assert!(matches!(
                check_access(Some(lo), false, true, Some(&hdr), None),
                Ok(Access::Device(_))
            ));

            // Push subscriptions ride along and vanish with the device.
            let sub =
                add_push_subscription(&dev.id, "https://web.push.apple.com/abc", "BKEY", "AUTH")
                    .unwrap();
            assert_eq!(push_subscriptions_for(&dev.id), vec![sub.clone()]);
            assert_eq!(list_devices()[0].push_subscriptions, 1);
            // Re-subscribing the same endpoint replaces, not duplicates.
            let sub2 =
                add_push_subscription(&dev.id, "https://web.push.apple.com/abc", "BKEY2", "AUTH2")
                    .unwrap();
            assert_eq!(push_subscriptions_for(&dev.id), vec![sub2.clone()]);
            // Another device can't remove it.
            assert!(!remove_push_subscription(&sub2.id, Some("dev-other")).unwrap());
            assert!(add_push_subscription("dev-nope", "https://x", "k", "a").is_err());

            // Revoke: token stops working, subscriptions gone.
            assert!(revoke_device(&dev.id).unwrap());
            assert!(!revoke_device(&dev.id).unwrap());
            assert_eq!(
                check_access(Some(lan), false, false, Some(&hdr), None),
                Err(Denied::Invalid)
            );
            assert!(list_push_subscriptions().is_empty());
        });
    }

    #[test]
    fn corrupt_registry_reads_as_empty() {
        with_temp_home(|_| {
            let p = devices_path().unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"{not json").unwrap();
            assert!(list_devices().is_empty());
            assert!(authenticate("anything").is_none());
        });
    }
}
