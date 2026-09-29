//! Web Push (RFC 8030 + RFC 8291 `aes128gcm` + RFC 8292 VAPID) for the
//! installed-PWA mobile client.
//!
//! No third-party web-push crate: everything is built from crates already in
//! the tree — `ring` (ECDH P-256, HMAC-SHA256 for HKDF, ECDSA P-256 for the
//! VAPID JWT) and `aes-gcm` (AES-128-GCM record encryption, the same crate the
//! key vault uses). The RFC 8291 Appendix A vector pins the derivation.
//!
//! - The VAPID keypair is generated once and kept in the encrypted key vault
//!   (`cortex-webpush` / `vapid-pkcs8`); only its public key is exposed
//!   (`GET /api/v2/push/vapid`).
//! - Subscriptions live in `~/.cortex/mobile-devices.json` next to the paired
//!   device that registered them (see `pairing.rs`).
//! - Egress is restricted to the browser vendors' push services
//!   ([`endpoint_allowed`]) — this sender never posts anywhere else, and a
//!   subscription with any other endpoint is refused at registration.
//! - Payloads are Declarative Web Push shaped (`{ web_push: 8030,
//!   notification: { title, body, navigate, app_badge } }`) plus a `cortex`
//!   block with the event / thread / run ids, so a service worker can either
//!   let the browser render it or route on the ids.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde::Serialize;
use serde_json::{json, Value};

use super::pairing::{self, PushSubscription};
use crate::commands::keyvault;

/// Key vault slot for the VAPID private key (PKCS#8, base64).
const VAULT_PROVIDER: &str = "cortex-webpush";
const VAULT_LABEL: &str = "vapid-pkcs8";

/// VAPID `sub` claim. A contact for the push service operator; the value
/// itself is not validated beyond its scheme by the services.
pub const VAPID_SUBJECT: &str = "mailto:cortex-mobile@localhost";

/// JWT lifetime (spec maximum is 24 h; 12 h keeps clock skew comfortable).
const JWT_TTL_SECS: i64 = 12 * 3600;

/// `aes128gcm` record size we advertise. One record per message; plaintext
/// is capped so it always fits with the delimiter + 16-byte tag.
const RECORD_SIZE: u32 = 4096;
const MAX_PLAINTEXT: usize = 3 * 1024;

/// Push-service `TTL` header (seconds the service may hold the message).
const PUSH_TTL_SECS: u32 = 24 * 3600;

const REQUEST_TIMEOUT_SECS: u64 = 15;

/// Exact hosts of the browser vendors' push services…
pub const ALLOWED_PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "web.push.apple.com",
    "updates.push.services.mozilla.com",
];
/// …plus Windows Notification Service, whose endpoints are per-tenant
/// subdomains (`wns2-par02p.notify.windows.com`).
pub const ALLOWED_PUSH_HOST_SUFFIXES: &[&str] = &[".notify.windows.com"];

/// Event names — the same vocabulary as `commands::push_notify`.
pub const EVENT_APPROVAL_NEEDED: &str = "approval_needed";
pub const EVENT_RUN_FINISHED: &str = "run_finished";
pub const EVENT_RUN_FAILED: &str = "run_failed";
pub const EVENT_QUOTA_LOW: &str = "quota_low";

// ───────────────────────────────────────────────────────────────────────────
// Endpoint validation
// ───────────────────────────────────────────────────────────────────────────

/// `scheme://host[:port]` of `url`, lowercased host; `None` when not a URL.
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

fn host_of(url: &str) -> Option<String> {
    let origin = origin_of(url)?;
    let (_, authority) = origin.split_once("://")?;
    let host = if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .map(|h| h.trim_start_matches('['))?
    } else {
        authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// The egress guard for this sender: HTTPS only, host must be one of the
/// browser push services. Applied at subscription time AND before every send.
pub fn endpoint_allowed(endpoint: &str) -> Result<(), String> {
    let endpoint = endpoint.trim();
    if !endpoint.starts_with("https://") {
        return Err("push endpoint must use https".into());
    }
    let host = host_of(endpoint).ok_or_else(|| "push endpoint has no host".to_string())?;
    if ALLOWED_PUSH_HOSTS.iter().any(|h| *h == host)
        || ALLOWED_PUSH_HOST_SUFFIXES
            .iter()
            .any(|suf| host.ends_with(suf))
    {
        Ok(())
    } else {
        Err(format!(
            "push endpoint host `{host}` is not a known browser push service"
        ))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// RFC 8291 content encryption
// ───────────────────────────────────────────────────────────────────────────

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&key, data).as_ref().to_vec()
}

/// HKDF-Extract (RFC 5869) with SHA-256.
fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    hmac_sha256(salt, ikm)
}

/// HKDF-Expand (RFC 5869) with SHA-256 to `len` bytes.
fn hkdf_expand(prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 32);
    let mut t: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < len {
        let mut block = t.clone();
        block.extend_from_slice(info);
        block.push(counter);
        t = hmac_sha256(prk, &block);
        out.extend_from_slice(&t);
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    out
}

/// The deterministic half of RFC 8291: given the ECDH shared secret, the
/// subscription's auth secret, both public keys and the salt, derive CEK +
/// nonce and produce the full `aes128gcm` body (header || single record).
/// Split from [`encrypt_payload`] so the RFC's test vector can pin it.
pub fn encrypt_with(
    ecdh_secret: &[u8],
    auth_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(format!(
            "push payload too large ({} bytes, cap {MAX_PLAINTEXT})",
            plaintext.len()
        ));
    }
    // IKM = HKDF(auth_secret, ecdh_secret, "WebPush: info" || 0x00 || ua_public || as_public, 32)
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let ikm = hkdf_expand(&hkdf_extract(auth_secret, ecdh_secret), &info, 32);
    // PRK = HKDF-Extract(salt, IKM); CEK / NONCE per RFC 8188.
    let prk = hkdf_extract(salt, &ikm);
    let cek = hkdf_expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf_expand(&prk, b"Content-Encoding: nonce\0", 12);

    // Single, final record: plaintext || 0x02 (last-record delimiter).
    let mut record = plaintext.to_vec();
    record.push(0x02);
    let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&cek));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), record.as_ref())
        .map_err(|_| "aes128gcm encrypt failed".to_string())?;

    // Header: salt(16) || rs(4, BE) || idlen(1) || keyid (= as_public).
    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

/// Encrypt `plaintext` for a subscription: fresh ephemeral P-256 key + salt,
/// ECDH against the subscription's `p256dh`, then [`encrypt_with`].
pub fn encrypt_payload(
    p256dh_b64: &str,
    auth_b64: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let ua_public = B64URL
        .decode(p256dh_b64.trim())
        .map_err(|e| format!("bad p256dh: {e}"))?;
    let auth_secret = B64URL
        .decode(auth_b64.trim())
        .map_err(|e| format!("bad auth secret: {e}"))?;
    if ua_public.len() != 65 || ua_public[0] != 0x04 {
        return Err("p256dh must be a 65-byte uncompressed P-256 point".into());
    }
    if auth_secret.len() != 16 {
        return Err("auth secret must be 16 bytes".into());
    }
    let rng = SystemRandom::new();
    let as_private = EphemeralPrivateKey::generate(&ECDH_P256, &rng)
        .map_err(|_| "ecdh keygen failed".to_string())?;
    let as_public = as_private
        .compute_public_key()
        .map_err(|_| "ecdh public key failed".to_string())?
        .as_ref()
        .to_vec();
    let peer = UnparsedPublicKey::new(&ECDH_P256, ua_public.as_slice());
    let ecdh_secret = agreement::agree_ephemeral(as_private, &peer, |s| s.to_vec())
        .map_err(|_| "ecdh agreement failed (invalid p256dh?)".to_string())?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| "rng failed".to_string())?;
    encrypt_with(
        &ecdh_secret,
        &auth_secret,
        &ua_public,
        &as_public,
        &salt,
        plaintext,
    )
}

// ───────────────────────────────────────────────────────────────────────────
// VAPID (RFC 8292)
// ───────────────────────────────────────────────────────────────────────────

/// Generate a fresh P-256 keypair as PKCS#8 bytes.
pub fn generate_pkcs8() -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map(|doc| doc.as_ref().to_vec())
        .map_err(|_| "vapid keygen failed".to_string())
}

/// Parse a PKCS#8 keypair.
pub fn keypair_from_pkcs8(pkcs8: &[u8]) -> Result<EcdsaKeyPair, String> {
    let rng = SystemRandom::new();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
        .map_err(|e| format!("vapid key rejected: {e}"))
}

/// The stored keypair, `Ok(None)` when none was generated yet. `Err` = vault
/// unreadable (locked keychain).
pub fn load_keypair() -> Result<Option<EcdsaKeyPair>, String> {
    let Some(b64) = keyvault::lookup_provider_key_sync(VAULT_PROVIDER)? else {
        return Ok(None);
    };
    let bytes = B64URL
        .decode(b64.trim())
        .map_err(|e| format!("stored vapid key is not base64: {e}"))?;
    keypair_from_pkcs8(&bytes).map(Some)
}

/// Load-or-create the VAPID keypair (first call mints + stores it).
pub async fn ensure_keypair() -> Result<EcdsaKeyPair, String> {
    if let Some(kp) = load_keypair()? {
        return Ok(kp);
    }
    let pkcs8 = generate_pkcs8()?;
    keyvault::vault_set(
        VAULT_PROVIDER.to_string(),
        VAULT_LABEL.to_string(),
        B64URL.encode(&pkcs8),
    )
    .await?;
    keypair_from_pkcs8(&pkcs8)
}

/// Base64url (unpadded) uncompressed public point — what the browser's
/// `pushManager.subscribe({ applicationServerKey })` takes.
pub fn public_key_b64(kp: &EcdsaKeyPair) -> String {
    B64URL.encode(kp.public_key().as_ref())
}

/// Build the VAPID JWT for `audience` (the push endpoint's origin).
pub fn vapid_jwt(kp: &EcdsaKeyPair, audience: &str, now_secs: i64) -> Result<String, String> {
    let header = B64URL.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = json!({
        "aud": audience,
        "exp": now_secs + JWT_TTL_SECS,
        "sub": VAPID_SUBJECT,
    });
    let claims = B64URL.encode(claims.to_string().as_bytes());
    let signing_input = format!("{header}.{claims}");
    let rng = SystemRandom::new();
    let sig = kp
        .sign(&rng, signing_input.as_bytes())
        .map_err(|_| "vapid sign failed".to_string())?;
    Ok(format!("{signing_input}.{}", B64URL.encode(sig.as_ref())))
}

/// `Authorization` header value: `vapid t=<jwt>, k=<public key>`.
pub fn vapid_authorization(kp: &EcdsaKeyPair, endpoint: &str) -> Result<String, String> {
    let aud = origin_of(endpoint).ok_or_else(|| "push endpoint is not a URL".to_string())?;
    let now = chrono::Utc::now().timestamp();
    let jwt = vapid_jwt(kp, &aud, now)?;
    Ok(format!("vapid t={jwt}, k={}", public_key_b64(kp)))
}

// ───────────────────────────────────────────────────────────────────────────
// Notification shaping + deep links
// ───────────────────────────────────────────────────────────────────────────

/// The HTTPS base the phone reaches this server on: the push config's
/// `mobile_url` override, else the live Tailscale MagicDNS name. `None` when
/// neither is known (plain LAN without a proxy).
pub fn server_https_base() -> Option<String> {
    let cfg = crate::commands::push_notify::load_config();
    let m = cfg.mobile_url.trim().trim_end_matches('/');
    if m.starts_with("https://") {
        return Some(m.to_string());
    }
    match crate::tailscale::current_status() {
        crate::tailscale::TsStatus::Connected { dnsname, .. } => {
            let d = dnsname.trim().trim_end_matches('.');
            (!d.is_empty()).then(|| format!("https://{d}"))
        }
        _ => None,
    }
}

/// Per-event deep links in both forms. `https` is `None` when the HTTPS base
/// is unknown; `scheme` (`cortex://…`) is always present.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeepLinks {
    pub https: Option<String>,
    pub scheme: String,
}

/// Deep-link route for an event: approvals open the approval, run outcomes
/// open the thread, quota opens the inbox.
pub fn deep_links(base_https: Option<&str>, event: &str, id: Option<&str>) -> DeepLinks {
    let route = match (event, id) {
        (EVENT_APPROVAL_NEEDED, Some(id)) => format!("approvals/{id}"),
        (EVENT_RUN_FINISHED | EVENT_RUN_FAILED, Some(id)) => format!("threads/{id}"),
        _ => "inbox".to_string(),
    };
    DeepLinks {
        https: base_https.map(|b| format!("{}/#/{route}", b.trim_end_matches('/'))),
        scheme: format!("cortex://{route}"),
    }
}

/// One notification to fan out to every subscription.
#[derive(Debug, Clone)]
pub struct Notification {
    pub event: &'static str,
    pub title: String,
    pub body: String,
    /// Thread (for run outcomes) or approval id — drives the deep link.
    pub target_id: Option<String>,
    pub thread_id: Option<String>,
    pub run_id: Option<String>,
    pub app_badge: Option<u32>,
}

/// Declarative-Web-Push-shaped payload.
pub fn payload_json(n: &Notification, base_https: Option<&str>) -> Value {
    let links = deep_links(base_https, n.event, n.target_id.as_deref());
    let navigate = links.https.clone().unwrap_or_else(|| links.scheme.clone());
    let mut notification = json!({
        "title": n.title,
        "body": n.body,
        "navigate": navigate,
    });
    if let Some(b) = n.app_badge {
        notification["app_badge"] = json!(b);
    }
    json!({
        "web_push": 8030,
        "notification": notification,
        "cortex": {
            "event": n.event,
            "thread_id": n.thread_id,
            "run_id": n.run_id,
            "deep_link": links,
        },
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Sending
// ───────────────────────────────────────────────────────────────────────────

fn limiter() -> &'static Mutex<crate::commands::push_notify::RateLimiter> {
    static L: once_cell::sync::Lazy<Mutex<crate::commands::push_notify::RateLimiter>> =
        once_cell::sync::Lazy::new(|| {
            Mutex::new(crate::commands::push_notify::RateLimiter::new(
                Duration::from_secs(300),
                Duration::from_secs(60),
                30,
            ))
        });
    &L
}

/// Outcome of one push-service POST.
#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub subscription_id: String,
    pub status: Option<u16>,
    pub ok: bool,
    pub error: Option<String>,
}

/// POST one encrypted message to one subscription. A 404/410 from the push
/// service means the subscription is dead and is purged.
pub async fn send_to(
    kp: &EcdsaKeyPair,
    sub: &PushSubscription,
    payload: &[u8],
    event: &str,
) -> SendOutcome {
    let fail = |error: String| SendOutcome {
        subscription_id: sub.id.clone(),
        status: None,
        ok: false,
        error: Some(error),
    };
    if let Err(e) = endpoint_allowed(&sub.endpoint) {
        return fail(e);
    }
    let body = match encrypt_payload(&sub.p256dh, &sub.auth, payload) {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let auth = match vapid_authorization(kp, &sub.endpoint) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    // Push services are public internet hosts: no Tailscale proxy needed, but
    // routing through it is harmless when the sidecar is up (exit node case).
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
    let client = match crate::tailscale::maybe_tailscale_proxy(builder).build() {
        Ok(c) => c,
        Err(e) => return fail(format!("client: {e}")),
    };
    let urgency = if event == EVENT_APPROVAL_NEEDED || event == EVENT_RUN_FAILED {
        "high"
    } else {
        "normal"
    };
    let res = client
        .post(&sub.endpoint)
        .header("Authorization", auth)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", PUSH_TTL_SECS.to_string())
        .header("Urgency", urgency)
        .header("Topic", event)
        .body(body)
        .send()
        .await;
    match res {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status == 404 || status == 410 {
                tracing::info!(
                    "webpush: subscription {} is gone ({status}); removing",
                    sub.id
                );
                pairing::remove_push_subscription_by_endpoint(&sub.endpoint);
            }
            SendOutcome {
                subscription_id: sub.id.clone(),
                status: Some(status),
                ok: (200..300).contains(&status),
                error: if (200..300).contains(&status) {
                    None
                } else {
                    Some(format!("http {status}"))
                },
            }
        }
        Err(e) => fail(format!("send: {e}")),
    }
}

/// Send `n` to every stored subscription. Returns per-subscription outcomes
/// (empty when nothing is subscribed or no VAPID key can be loaded).
pub async fn send_all(n: &Notification) -> Vec<SendOutcome> {
    let subs = pairing::list_push_subscriptions();
    if subs.is_empty() {
        return Vec::new();
    }
    let kp = match load_keypair() {
        Ok(Some(kp)) => kp,
        Ok(None) => {
            tracing::debug!("webpush: no VAPID key yet; nothing sent");
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!("webpush: vault unreadable: {e}");
            return Vec::new();
        }
    };
    let payload = payload_json(n, server_https_base().as_deref()).to_string();
    let mut out = Vec::with_capacity(subs.len());
    for sub in &subs {
        out.push(send_to(&kp, sub, payload.as_bytes(), n.event).await);
    }
    out
}

/// Non-blocking, best-effort fan-out: rate-limited per `dedupe_key`, runs on
/// its own OS thread with a current-thread runtime so it is safe to call from
/// sync code inside or outside a tokio runtime (same shape as
/// `push_notify::fire_detached`).
pub fn fire_detached(dedupe_key: String, n: Notification) {
    {
        let mut l = limiter().lock().unwrap_or_else(|e| e.into_inner());
        if !l.allow(&format!("{}:{dedupe_key}", n.event), Instant::now()) {
            tracing::debug!("webpush: rate-limited {} ({dedupe_key})", n.event);
            return;
        }
    }
    // Cheap early-out before spawning a thread: no subscriptions, no work.
    if pairing::list_push_subscriptions().is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("cortex-webpush-send".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("webpush: runtime: {e}");
                    return;
                }
            };
            for r in rt.block_on(send_all(&n)) {
                if !r.ok {
                    tracing::warn!(
                        "webpush: {} to {} failed: status={:?} err={:?}",
                        n.event,
                        r.subscription_id,
                        r.status,
                        r.error
                    );
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("webpush: could not spawn sender thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey as VerifyKey, ECDSA_P256_SHA256_FIXED};

    fn d(s: &str) -> Vec<u8> {
        B64URL.decode(s).unwrap()
    }

    /// RFC 8291 Appendix A. The ECDH step is ring's (no way to inject the
    /// fixed private scalar), so the vector pins everything from the shared
    /// secret onwards: IKM/PRK/CEK/nonce derivation, padding, AES-GCM and the
    /// `aes128gcm` header layout.
    #[test]
    fn rfc8291_appendix_a_vector() {
        let plaintext = b"When I grow up, I want to be a watermelon";
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ecdh_secret = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let auth_secret = d("BTBZMqHH6r4Tts7J_aSIgg");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();

        // Intermediate values (A.2).
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(&ua_public);
        info.extend_from_slice(&as_public);
        let ikm = hkdf_expand(&hkdf_extract(&auth_secret, &ecdh_secret), &info, 32);
        assert_eq!(
            B64URL.encode(&ikm),
            "S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg"
        );
        let prk = hkdf_extract(&salt, &ikm);
        assert_eq!(
            B64URL.encode(&prk),
            "09_eUZGrsvxChDCGRCdkLiDXrReGOEVeSCdCcPBSJSc"
        );
        let cek = hkdf_expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
        assert_eq!(B64URL.encode(&cek), "oIhVW04MRdy2XN9CiKLxTg");
        let nonce = hkdf_expand(&prk, b"Content-Encoding: nonce\0", 12);
        assert_eq!(B64URL.encode(&nonce), "4h_95klXJ5E_qnoN");

        // Output (A.3).
        let body = encrypt_with(
            &ecdh_secret,
            &auth_secret,
            &ua_public,
            &as_public,
            &salt,
            plaintext,
        )
        .unwrap();
        assert_eq!(
            B64URL.encode(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
        // Header layout: salt, rs=4096, idlen=65, keyid.
        assert_eq!(&body[..16], &salt);
        assert_eq!(&body[16..20], &4096u32.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(&body[21..86], &as_public[..]);
    }

    #[test]
    fn hkdf_expand_spans_blocks() {
        // 40 bytes needs two HMAC blocks; the first 32 must equal a 32-byte expand.
        let prk = hkdf_extract(b"salt", b"ikm");
        let a = hkdf_expand(&prk, b"info", 32);
        let b = hkdf_expand(&prk, b"info", 40);
        assert_eq!(&b[..32], &a[..]);
        assert_eq!(b.len(), 40);
    }

    #[test]
    fn encrypt_payload_round_trips_against_a_fresh_ua_key() {
        // Simulate the browser: a UA keypair + auth secret, subscribe with
        // the public half, then decrypt what the server produced.
        let rng = SystemRandom::new();
        let ua_private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
        let ua_public = ua_private.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        let plaintext = br#"{"web_push":8030,"notification":{"title":"hi"}}"#;
        let body =
            encrypt_payload(&B64URL.encode(&ua_public), &B64URL.encode(auth), plaintext).unwrap();

        // Parse the header back out.
        let salt = &body[..16];
        let idlen = body[20] as usize;
        let as_public = &body[21..21 + idlen];
        let ciphertext = &body[21 + idlen..];
        let peer = UnparsedPublicKey::new(&ECDH_P256, as_public);
        let secret = agreement::agree_ephemeral(ua_private, &peer, |s| s.to_vec()).unwrap();
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(&ua_public);
        info.extend_from_slice(as_public);
        let ikm = hkdf_expand(&hkdf_extract(&auth, &secret), &info, 32);
        let prk = hkdf_extract(salt, &ikm);
        let cek = hkdf_expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
        let nonce = hkdf_expand(&prk, b"Content-Encoding: nonce\0", 12);
        let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&cek));
        let mut record = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext)
            .expect("decrypts");
        assert_eq!(record.pop(), Some(0x02));
        assert_eq!(record, plaintext);
    }

    #[test]
    fn encrypt_payload_rejects_bad_client_keys() {
        assert!(encrypt_payload("not-base64!", "AAAAAAAAAAAAAAAAAAAAAA", b"x").is_err());
        assert!(encrypt_payload(&B64URL.encode([0x04u8; 65]), "AAAA", b"x").is_err());
        let big = vec![b'x'; MAX_PLAINTEXT + 1];
        assert!(encrypt_with(&[0; 32], &[0; 16], &[4; 65], &[4; 65], &[0; 16], &big).is_err());
    }

    #[test]
    fn vapid_jwt_is_es256_and_verifies_with_the_public_key() {
        let pkcs8 = generate_pkcs8().unwrap();
        let kp = keypair_from_pkcs8(&pkcs8).unwrap();
        let jwt = vapid_jwt(&kp, "https://web.push.apple.com", 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&d(parts[0])).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["typ"], "JWT");
        let claims: Value = serde_json::from_slice(&d(parts[1])).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], 1_700_000_000 + JWT_TTL_SECS);
        assert_eq!(claims["sub"], VAPID_SUBJECT);
        // Fixed-size (r||s) signature verifiable with the raw public point.
        let sig = d(parts[2]);
        assert_eq!(sig.len(), 64);
        let pubkey = d(&public_key_b64(&kp));
        assert_eq!(pubkey.len(), 65);
        assert_eq!(pubkey[0], 0x04);
        VerifyKey::new(&ECDSA_P256_SHA256_FIXED, &pubkey)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .expect("signature verifies");
        let auth = vapid_authorization(&kp, "https://web.push.apple.com/QDxZ").unwrap();
        assert!(auth.starts_with("vapid t="));
        assert!(auth.contains(", k="));
    }

    #[test]
    fn endpoint_allowlist() {
        assert!(endpoint_allowed("https://fcm.googleapis.com/fcm/send/abc").is_ok());
        assert!(endpoint_allowed("https://web.push.apple.com/QDxZ").is_ok());
        assert!(endpoint_allowed("https://updates.push.services.mozilla.com/wpush/v2/x").is_ok());
        assert!(endpoint_allowed("https://wns2-par02p.notify.windows.com/w/?token=x").is_ok());
        assert!(endpoint_allowed("http://fcm.googleapis.com/x").is_err());
        assert!(endpoint_allowed("https://fcm.googleapis.com.evil.example/x").is_err());
        assert!(endpoint_allowed("https://evilnotify.windows.com/x").is_err());
        assert!(endpoint_allowed("https://127.0.0.1/x").is_err());
        assert!(endpoint_allowed("https://user@web.push.apple.com/x").is_ok());
        assert_eq!(
            origin_of("https://Web.Push.Apple.com:443/QDxZ?x=1"),
            Some("https://web.push.apple.com:443".to_string())
        );
    }

    #[test]
    fn deep_links_and_payload_shape() {
        let l = deep_links(
            Some("https://cortex.tail1.ts.net/"),
            EVENT_APPROVAL_NEEDED,
            Some("ap-1"),
        );
        assert_eq!(
            l.https.as_deref(),
            Some("https://cortex.tail1.ts.net/#/approvals/ap-1")
        );
        assert_eq!(l.scheme, "cortex://approvals/ap-1");
        let l = deep_links(None, EVENT_RUN_FAILED, Some("th-1"));
        assert_eq!(l.https, None);
        assert_eq!(l.scheme, "cortex://threads/th-1");
        assert_eq!(
            deep_links(None, EVENT_QUOTA_LOW, None).scheme,
            "cortex://inbox"
        );

        let n = Notification {
            event: EVENT_RUN_FINISHED,
            title: "Run finished".into(),
            body: "claude-cli".into(),
            target_id: Some("th-1".into()),
            thread_id: Some("th-1".into()),
            run_id: Some("run-1".into()),
            app_badge: Some(2),
        };
        let p = payload_json(&n, Some("https://h"));
        assert_eq!(p["web_push"], 8030);
        assert_eq!(p["notification"]["title"], "Run finished");
        assert_eq!(p["notification"]["navigate"], "https://h/#/threads/th-1");
        assert_eq!(p["notification"]["app_badge"], 2);
        assert_eq!(p["cortex"]["event"], "run_finished");
        assert_eq!(p["cortex"]["deep_link"]["scheme"], "cortex://threads/th-1");
        // No https base → navigate falls back to the scheme link.
        let p = payload_json(&n, None);
        assert_eq!(p["notification"]["navigate"], "cortex://threads/th-1");
    }
}
