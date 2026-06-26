//! Browser cookie extraction for web-session auto-detect.
//!
//! Chat history lives behind a provider's *web* session, which the API/CLI
//! login does not include. To avoid making the user paste a token, we read the
//! session cookie straight out of an installed browser's cookie store.
//!
//! ## Windows (priority target — the user's machine)
//! Chromium browsers (Chrome, Edge) encrypt cookie values with AES-256-GCM
//! under a per-profile key that is itself DPAPI-protected:
//!
//! 1. Read `…\User Data\Local State` → JSON `os_crypt.encrypted_key` (base64).
//! 2. base64-decode, strip the 5-byte `"DPAPI"` prefix.
//! 3. DPAPI-decrypt (`CryptUnprotectData`) → the raw 32-byte AES key.
//! 4. Read `…\User Data\Default\Network\Cookies` (SQLite). Each encrypted value
//!    is `b"v10"` + 12-byte nonce + ciphertext + 16-byte GCM tag.
//! 5. AES-256-GCM decrypt with the key+nonce → the plaintext cookie value.
//!
//! Chrome holds an exclusive lock on the Cookies DB while running, so we copy it
//! to a temp file before opening it with rusqlite.
//!
//! ## Non-Windows
//! Linux/macOS Chromium key derivation differs (libsecret / Keychain) and is out
//! of scope for this priority pass. We provide a best-effort **Firefox** reader
//! (`cookies.sqlite` stores values in plaintext) and otherwise return a clear
//! "unsupported" error rather than panicking — so the login fallback kicks in.
//!
//! **No cookie value or key is ever logged.** Errors carry only structural info.

use std::path::{Path, PathBuf};

/// A web provider whose session cookie we know how to locate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebProvider {
    Claude,
    ChatGpt,
}

impl WebProvider {
    /// The cookie domains that hold this provider's session, most-preferred
    /// first. ChatGPT migrated from `chat.openai.com` to `chatgpt.com`.
    fn domains(self) -> &'static [&'static str] {
        match self {
            WebProvider::Claude => &["claude.ai"],
            WebProvider::ChatGpt => &["chatgpt.com", "chat.openai.com"],
        }
    }

    /// The cookie name carrying the session credential.
    fn cookie_name(self) -> &'static str {
        match self {
            WebProvider::Claude => "sessionKey",
            // ChatGPT's web session cookie; exchanged for an accessToken later.
            WebProvider::ChatGpt => "__Secure-next-auth.session-token",
        }
    }

    /// Canonical provider key used by the rest of the module / config.
    pub fn key(self) -> &'static str {
        match self {
            WebProvider::Claude => "claude",
            WebProvider::ChatGpt => "chatgpt",
        }
    }
}

/// Attempt to auto-detect the given provider's web-session cookie from any
/// supported installed browser. Returns the raw cookie value on success.
///
/// Tries browsers in a sensible order and returns the first hit. On every
/// platform/browser failing, returns a single human-readable error so the
/// caller can fall back to the in-app login flow.
pub fn detect_session_cookie(provider: WebProvider) -> Result<String, String> {
    #[cfg(windows)]
    {
        windows_impl::detect(provider)
    }
    #[cfg(not(windows))]
    {
        non_windows_impl::detect(provider)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// AES-256-GCM decrypt of a Chromium `v10` cookie value. Shared + unit-tested on
// every platform (it's pure crypto, no OS calls).
// ───────────────────────────────────────────────────────────────────────────

/// Decrypt a Chromium-encrypted cookie blob given the 32-byte `os_crypt` key.
///
/// Layout: `b"v10"` (3) + nonce (12) + ciphertext + tag (16). The `aes-gcm`
/// crate expects the tag appended to the ciphertext, which matches Chromium's
/// layout exactly, so we feed it `ciphertext || tag` directly.
///
/// Returns the UTF-8 plaintext value, or an error string (never the bytes).
pub fn decrypt_chromium_value(key: &[u8], blob: &[u8]) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    if key.len() != 32 {
        return Err(format!("aes key wrong length: {} (want 32)", key.len()));
    }
    // 3-byte version prefix + 12-byte nonce + at least a 16-byte tag.
    if blob.len() < 3 + 12 + 16 {
        return Err("cookie blob too short to be a v10/v11 value".to_string());
    }
    let prefix = &blob[0..3];
    if prefix != b"v10" && prefix != b"v11" {
        return Err("cookie value is not Chromium v10/v11 (unencrypted or unknown)".to_string());
    }
    let nonce = &blob[3..15];
    let ct_and_tag = &blob[15..];

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ct_and_tag)
        .map_err(|_| "AES-GCM decrypt failed (wrong key or corrupt value)".to_string())?;
    String::from_utf8(plaintext).map_err(|_| "decrypted cookie value was not valid UTF-8".to_string())
}

/// Pull the base64 `os_crypt.encrypted_key` out of a Chromium `Local State`
/// JSON file and strip the 5-byte `"DPAPI"` prefix, returning the still-DPAPI-
/// encrypted key bytes. (DPAPI decryption itself is Windows-only.)
// Only invoked from the Windows path; exercised by unit tests on all platforms.
#[cfg_attr(not(windows), allow(dead_code))]
fn read_dpapi_wrapped_key(local_state_path: &Path) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let bytes = std::fs::read(local_state_path)
        .map_err(|e| format!("read Local State: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse Local State JSON: {e}"))?;
    let b64 = json
        .get("os_crypt")
        .and_then(|o| o.get("encrypted_key"))
        .and_then(|k| k.as_str())
        .ok_or_else(|| "Local State missing os_crypt.encrypted_key".to_string())?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("base64-decode encrypted_key: {e}"))?;
    if raw.len() <= 5 || &raw[0..5] != b"DPAPI" {
        return Err("encrypted_key missing expected DPAPI prefix".to_string());
    }
    Ok(raw[5..].to_vec())
}

/// Copy the (possibly locked) Cookies SQLite DB to a fresh temp file and read
/// the encrypted blob for the first matching `(domain, name)`. Returns the raw
/// encrypted bytes (`v10…`), to be decrypted by the caller.
///
/// Chrome keeps an exclusive lock on the live DB; copying first sidesteps it.
// Only invoked from the Windows path.
#[cfg_attr(not(windows), allow(dead_code))]
fn read_encrypted_cookie_blob(
    cookies_db: &Path,
    domains: &[&str],
    name: &str,
) -> Result<Vec<u8>, String> {
    // Copy to a temp file so we don't fight Chrome's lock on the live DB.
    let tmp = std::env::temp_dir().join(format!("cortex-cookies-{}.db", std::process::id()));
    std::fs::copy(cookies_db, &tmp).map_err(|e| format!("copy Cookies DB: {e}"))?;
    // Best-effort cleanup guard.
    let _guard = TempFileGuard(tmp.clone());

    let conn = rusqlite::Connection::open_with_flags(
        &tmp,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open Cookies DB: {e}"))?;

    // host_key is stored with or without a leading dot; match both. Newest
    // (longest-lived) first so we prefer a still-valid session cookie.
    for domain in domains {
        let mut stmt = conn
            .prepare(
                "SELECT encrypted_value FROM cookies
                 WHERE name = ?1 AND (host_key = ?2 OR host_key = ?3)
                 ORDER BY expires_utc DESC LIMIT 1",
            )
            .map_err(|e| format!("prepare cookie query: {e}"))?;
        let dotted = format!(".{domain}");
        let blob: Option<Vec<u8>> = stmt
            .query_row(rusqlite::params![name, domain, dotted], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .ok();
        if let Some(b) = blob {
            if !b.is_empty() {
                return Ok(b);
            }
        }
    }
    Err(format!(
        "no '{name}' cookie found for domains {domains:?} (not logged in?)"
    ))
}

/// Deletes a temp file when dropped (best-effort).
struct TempFileGuard(PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Firefox reader (shared, ALL platforms). Firefox stores cookie values in
// plaintext in `cookies.sqlite`, so no key derivation is needed. Firefox is a
// very common default browser, so Windows tries it too — after Chromium —
// rather than treating it as a non-Windows-only fallback.
// ───────────────────────────────────────────────────────────────────────────

mod firefox {
    use super::*;

    /// First Firefox profile that yields the provider's session cookie.
    pub fn detect(provider: WebProvider) -> Result<String, String> {
        let base = profiles_dir().ok_or_else(|| "no Firefox profiles dir".to_string())?;
        let entries =
            std::fs::read_dir(&base).map_err(|e| format!("read Firefox profiles: {e}"))?;
        let mut last = String::from("no Firefox profile had the cookie");
        for entry in entries.flatten() {
            let db = entry.path().join("cookies.sqlite");
            if !db.exists() {
                continue;
            }
            match read(&db, provider.domains(), provider.cookie_name()) {
                Ok(v) => return Ok(v),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Per-platform Firefox profiles directory.
    fn profiles_dir() -> Option<PathBuf> {
        #[cfg(windows)]
        {
            // %APPDATA%\Mozilla\Firefox\Profiles
            Some(
                dirs::config_dir()?
                    .join("Mozilla")
                    .join("Firefox")
                    .join("Profiles"),
            )
        }
        #[cfg(not(windows))]
        {
            let home = dirs::home_dir()?;
            let linux = home.join(".mozilla/firefox");
            if linux.exists() {
                return Some(linux);
            }
            let mac = home.join("Library/Application Support/Firefox/Profiles");
            if mac.exists() {
                return Some(mac);
            }
            None
        }
    }

    /// Read the plaintext cookie value for the first matching `(host, name)`.
    fn read(db: &Path, domains: &[&str], name: &str) -> Result<String, String> {
        let tmp = std::env::temp_dir().join(format!("cortex-ff-{}.db", std::process::id()));
        std::fs::copy(db, &tmp).map_err(|e| format!("copy cookies.sqlite: {e}"))?;
        let _guard = TempFileGuard(tmp.clone());
        let conn = rusqlite::Connection::open_with_flags(
            &tmp,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open cookies.sqlite: {e}"))?;
        for domain in domains {
            let dotted = format!(".{domain}");
            let mut stmt = conn
                .prepare(
                    "SELECT value FROM moz_cookies
                     WHERE name = ?1 AND (host = ?2 OR host = ?3)
                     ORDER BY expiry DESC LIMIT 1",
                )
                .map_err(|e| format!("prepare: {e}"))?;
            let v: Option<String> = stmt
                .query_row(rusqlite::params![name, domain, dotted], |r| r.get(0))
                .ok();
            if let Some(v) = v.filter(|s| !s.is_empty()) {
                return Ok(v);
            }
        }
        Err(format!("no '{name}' cookie for {domains:?} in Firefox"))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Windows implementation
// ───────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod windows_impl {
    use super::*;

    /// A Chromium browser install: where its `User Data` lives.
    struct Browser {
        name: &'static str,
        /// Path under %LOCALAPPDATA% to the `User Data` dir.
        rel: &'static str,
    }

    const BROWSERS: &[Browser] = &[
        Browser { name: "Chrome", rel: r"Google\Chrome\User Data" },
        Browser { name: "Edge", rel: r"Microsoft\Edge\User Data" },
    ];

    pub fn detect(provider: WebProvider) -> Result<String, String> {
        let local_appdata = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "LOCALAPPDATA not set".to_string())?;

        let mut last_err = String::from("no supported browser yielded a session cookie");
        for b in BROWSERS {
            let user_data = local_appdata.join(b.rel);
            let local_state = user_data.join("Local State");
            let cookies_db = user_data.join(r"Default\Network\Cookies");
            if !local_state.exists() || !cookies_db.exists() {
                continue;
            }
            match extract_from(&local_state, &cookies_db, provider) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = format!("{}: {e}", b.name),
            }
        }
        // Chromium browsers didn't yield it — try Firefox (a very common default
        // browser on Windows; its cookies.sqlite is plaintext, no DPAPI needed).
        match super::firefox::detect(provider) {
            Ok(v) => Ok(v),
            Err(e) => Err(format!("{last_err}; Firefox: {e}")),
        }
    }

    fn extract_from(
        local_state: &Path,
        cookies_db: &Path,
        provider: WebProvider,
    ) -> Result<String, String> {
        let wrapped = read_dpapi_wrapped_key(local_state)?;
        let key = dpapi_decrypt(&wrapped)?;
        let blob = read_encrypted_cookie_blob(cookies_db, provider.domains(), provider.cookie_name())?;
        decrypt_chromium_value(&key, &blob)
    }

    /// DPAPI-decrypt the (de-prefixed) `encrypted_key` into the raw AES key via
    /// `CryptUnprotectData`. Current-user scope, no entropy — matching Chromium.
    fn dpapi_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

        // SAFETY: we pass a valid input blob and read back the out blob's
        // pointer/length, then copy and free it. `pbData` is non-null on success.
        unsafe {
            let mut in_blob = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut out_blob = CRYPT_INTEGER_BLOB::default();

            CryptUnprotectData(
                &mut in_blob,
                None,
                None,
                None,
                None,
                0,
                &mut out_blob,
            )
            .map_err(|e| format!("CryptUnprotectData failed: {}", e.code().0))?;

            if out_blob.pbData.is_null() {
                return Err("CryptUnprotectData returned null".to_string());
            }
            let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
            let key = slice.to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
            Ok(key)
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Non-Windows implementation: best-effort Firefox (plaintext cookies.sqlite).
// ───────────────────────────────────────────────────────────────────────────

#[cfg(not(windows))]
mod non_windows_impl {
    use super::*;

    pub fn detect(provider: WebProvider) -> Result<String, String> {
        match super::firefox::detect(provider) {
            Ok(v) => Ok(v),
            Err(e) => Err(format!(
                "browser auto-detect unsupported on this platform for {} \
                 (Chromium key derivation is Windows-only here); Firefox fallback: {e}",
                provider.key()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    /// Build a Chromium-style `v10` blob from a known key+nonce+plaintext, then
    /// verify [`decrypt_chromium_value`] recovers the plaintext. This is the
    /// load-bearing crypto path that runs on the real machine.
    #[test]
    fn aes_gcm_v10_roundtrip_known_fixture() {
        // Fixed, non-secret test key/nonce (32 + 12 bytes).
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        let plaintext = b"sk-test-session-cookie-value";

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let ct_and_tag = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .expect("encrypt");

        let mut blob = Vec::new();
        blob.extend_from_slice(b"v10");
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct_and_tag);

        let got = decrypt_chromium_value(&key, &blob).expect("decrypt");
        assert_eq!(got, "sk-test-session-cookie-value");
    }

    #[test]
    fn rejects_wrong_key_length() {
        assert!(decrypt_chromium_value(&[0u8; 16], &[0u8; 64]).is_err());
    }

    #[test]
    fn rejects_short_blob() {
        assert!(decrypt_chromium_value(&[0u8; 32], b"v10short").is_err());
    }

    #[test]
    fn rejects_non_v10_prefix() {
        let blob = {
            let mut b = vec![b'x', b'9', b'9'];
            b.extend_from_slice(&[0u8; 12 + 16 + 4]);
            b
        };
        assert!(decrypt_chromium_value(&[0u8; 32], &blob).is_err());
    }

    #[test]
    fn wrong_key_fails_auth_tag() {
        let key = [1u8; 32];
        let nonce = [2u8; 12];
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), b"hi".as_ref())
            .unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        // Decrypt with a different key → GCM tag mismatch → error.
        assert!(decrypt_chromium_value(&[2u8; 32], &blob).is_err());
    }

    #[test]
    fn read_dpapi_wrapped_key_strips_prefix() {
        use base64::Engine as _;
        let mut raw = b"DPAPI".to_vec();
        raw.extend_from_slice(&[1, 2, 3, 4]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let json = serde_json::json!({ "os_crypt": { "encrypted_key": b64 } });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Local State");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let got = read_dpapi_wrapped_key(&path).unwrap();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_dpapi_wrapped_key_rejects_missing_prefix() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"NODPAPI");
        let json = serde_json::json!({ "os_crypt": { "encrypted_key": b64 } });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Local State");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(read_dpapi_wrapped_key(&path).is_err());
    }

    #[test]
    fn provider_metadata() {
        assert_eq!(WebProvider::Claude.cookie_name(), "sessionKey");
        assert_eq!(WebProvider::Claude.key(), "claude");
        assert!(WebProvider::ChatGpt.domains().contains(&"chatgpt.com"));
    }
}
