//! Encrypted provider key vault.
//!
//! Stores per-provider API keys (anthropic, openai, gemini, etc.) in an
//! AES-256-GCM-encrypted JSON file at `~/.cortex/keys.enc`. The 32-byte
//! master key lives in the OS keychain under the `com.networkbound.cortex` service
//! with username `keyvault`. On first use we generate a random key and
//! persist it; subsequent calls reuse it.
//!
//! On-disk layout (binary, little-endian):
//!
//! ```text
//! [12 bytes: nonce] [N bytes: AES-GCM ciphertext+tag]
//! ```
//!
//! Plaintext is a JSON array of `KeyEntry`. Keeping it as a single envelope
//! (rather than a per-key file) lets us re-encrypt atomically: we write to
//! `keys.enc.tmp` then rename, so an interrupted update can never leave a
//! half-written vault on disk.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "com.networkbound.cortex";
const KEYRING_USER_MASTER: &str = "keyvault";
const VAULT_FILENAME: &str = "keys.enc";
const NONCE_LEN: usize = 12;
const MASTER_KEY_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    pub provider: String,
    pub label: String,
    pub key: String,
    pub added_unix_ms: i64,
}

/// Metadata variant — the `key` field is intentionally stripped so a UI
/// list can render safely without ever pulling secrets across the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMetadata {
    pub provider: String,
    pub label: String,
    pub added_unix_ms: i64,
}

impl From<&KeyEntry> for KeyMetadata {
    fn from(e: &KeyEntry) -> Self {
        Self {
            provider: e.provider.clone(),
            label: e.label.clone(),
            added_unix_ms: e.added_unix_ms,
        }
    }
}

fn vault_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    Ok(home.join(".cortex"))
}

fn vault_path() -> Result<PathBuf, String> {
    Ok(vault_dir()?.join(VAULT_FILENAME))
}

/// Generate a fresh 32-byte master key. Uses the OS RNG via `getrandom`,
/// which `aes-gcm`'s `OsRng` re-exports through `aead::rand_core`.
fn generate_master_key() -> [u8; MASTER_KEY_LEN] {
    use aes_gcm::aead::rand_core::RngCore;
    let mut out = [0u8; MASTER_KEY_LEN];
    aes_gcm::aead::OsRng.fill_bytes(&mut out);
    out
}

/// Returns the master key, generating + persisting one on first use.
/// Stored as base64 so the keychain doesn't have to deal with raw bytes
/// (some platforms reject NUL-containing strings).
fn get_or_init_master() -> Result<[u8; MASTER_KEY_LEN], String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_MASTER)
        .map_err(|e| format!("keyring entry: {e}"))?;
    if let Ok(existing) = entry.get_password() {
        let decoded = base64_decode(&existing)
            .map_err(|e| format!("decode master key: {e}"))?;
        if decoded.len() != MASTER_KEY_LEN {
            return Err(format!(
                "master key wrong length: expected {MASTER_KEY_LEN}, got {}",
                decoded.len()
            ));
        }
        let mut buf = [0u8; MASTER_KEY_LEN];
        buf.copy_from_slice(&decoded);
        return Ok(buf);
    }
    // First-run: mint a new one and save.
    let fresh = generate_master_key();
    let encoded = base64_encode(&fresh);
    entry
        .set_password(&encoded)
        .map_err(|e| format!("keyring set: {e}"))?;
    Ok(fresh)
}

/// Minimal base64 encode using the standard alphabet without padding being
/// required on decode. Kept local because we don't want to pull a full
/// base64 crate just for ~50 bytes of master-key shuffling.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        out.push(ALPHABET[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("bad base64 char: {c}")),
        }
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|c| !matches!(*c, b'\n' | b'\r' | b' ' | b'\t'))
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let mut buf = [0u8; 4];
        let mut pad = 0;
        for j in 0..4 {
            if bytes[i + j] == b'=' {
                pad += 1;
                buf[j] = 0;
            } else {
                buf[j] = val(bytes[i + j])?;
            }
        }
        let n = ((buf[0] as u32) << 18)
            | ((buf[1] as u32) << 12)
            | ((buf[2] as u32) << 6)
            | (buf[3] as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
        i += 4;
    }
    Ok(out)
}

/// Read + decrypt the on-disk vault. Returns an empty list when the file
/// doesn't exist yet (first-run UX).
fn load_entries() -> Result<Vec<KeyEntry>, String> {
    let path = vault_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut f = fs::File::open(&path).map_err(|e| format!("open vault: {e}"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("read vault: {e}"))?;
    if buf.len() < NONCE_LEN {
        return Err("vault file too short".into());
    }
    let (nonce_bytes, ct) = buf.split_at(NONCE_LEN);
    let master = get_or_init_master()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&master));
    let nonce = Nonce::from_slice(nonce_bytes);
    let pt = cipher
        .decrypt(nonce, ct)
        .map_err(|e| format!("decrypt vault: {e}"))?;
    let entries: Vec<KeyEntry> = serde_json::from_slice(&pt)
        .map_err(|e| format!("parse vault json: {e}"))?;
    Ok(entries)
}

/// Encrypt + atomically write `entries` to the vault path. The temp+rename
/// dance avoids leaving a half-encrypted file on crash.
fn save_entries(entries: &[KeyEntry]) -> Result<(), String> {
    let dir = vault_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir vault dir: {e}"))?;
    let pt = serde_json::to_vec(entries).map_err(|e| format!("encode vault json: {e}"))?;
    let master = get_or_init_master()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&master));

    use aes_gcm::aead::rand_core::RngCore;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    aes_gcm::aead::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, pt.as_ref())
        .map_err(|e| format!("encrypt vault: {e}"))?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct);

    let path = vault_path()?;
    let tmp = path.with_extension("enc.tmp");
    fs::write(&tmp, &blob).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename vault: {e}"))?;
    Ok(())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tauri::command]
pub async fn vault_list() -> Result<Vec<KeyMetadata>, String> {
    let entries = load_entries()?;
    Ok(entries.iter().map(KeyMetadata::from).collect())
}

#[tauri::command]
pub async fn vault_get(provider: String, label: String) -> Result<String, String> {
    let entries = load_entries()?;
    entries
        .into_iter()
        .find(|e| e.provider == provider && e.label == label)
        .map(|e| e.key)
        .ok_or_else(|| format!("no key for {provider}/{label}"))
}

/// Synchronous variant for callers that need to resolve a key inside a
/// non-async context (e.g. header substitution inside the tool virtualizer
/// invoke pipeline). Returns the raw key string or an error.
pub fn lookup_key_sync(provider: &str, label: &str) -> Result<String, String> {
    let entries = load_entries()?;
    entries
        .into_iter()
        .find(|e| e.provider == provider && e.label == label)
        .map(|e| e.key)
        .ok_or_else(|| format!("no key for {provider}/{label}"))
}

#[tauri::command]
pub async fn vault_set(
    provider: String,
    label: String,
    key: String,
) -> Result<(), String> {
    if provider.trim().is_empty() {
        return Err("provider must not be empty".into());
    }
    if label.trim().is_empty() {
        return Err("label must not be empty".into());
    }
    let mut entries = load_entries()?;
    // Upsert on (provider, label) so re-saving updates the key + timestamp.
    if let Some(existing) = entries
        .iter_mut()
        .find(|e| e.provider == provider && e.label == label)
    {
        existing.key = key;
        existing.added_unix_ms = now_ms();
    } else {
        entries.push(KeyEntry {
            provider,
            label,
            key,
            added_unix_ms: now_ms(),
        });
    }
    save_entries(&entries)?;
    Ok(())
}

#[tauri::command]
pub async fn vault_remove(provider: String, label: String) -> Result<(), String> {
    let mut entries = load_entries()?;
    let before = entries.len();
    entries.retain(|e| !(e.provider == provider && e.label == label));
    if entries.len() == before {
        return Err(format!("no key for {provider}/{label}"));
    }
    save_entries(&entries)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let cases: &[&[u8]] = &[b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"];
        for c in cases {
            let enc = base64_encode(c);
            let dec = base64_decode(&enc).unwrap();
            assert_eq!(&dec[..], *c);
        }
    }

    #[test]
    fn metadata_strips_key() {
        let e = KeyEntry {
            provider: "anthropic".into(),
            label: "personal".into(),
            key: "sk-secret".into(),
            added_unix_ms: 123,
        };
        let md = KeyMetadata::from(&e);
        let json = serde_json::to_string(&md).unwrap();
        assert!(!json.contains("sk-secret"));
        assert!(json.contains("anthropic"));
    }
}
