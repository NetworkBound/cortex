//! In-app self-update for the Linux AppImage build.
//!
//! Unlike `updater.rs` (which only CHECKS a manifest), this module actually
//! downloads a newer AppImage from the Gitea release and swaps the running
//! AppImage in place. The replace applies the next time Cortex launches — a
//! running process keeps its old inode, so swapping mid-use is safe.
//!
//! Strictly fail-safe:
//!   * Only ever active when the process is an AppImage (`$APPIMAGE` set) on
//!     Linux. On Windows / .deb / dev runs the commands report `supported:
//!     false` and do nothing — so this can never affect the Windows build.
//!   * **Every downloaded AppImage must carry a valid ed25519 signature** over
//!     its bytes, made by the release private key and verified against the
//!     baked-in public key BEFORE the binary is written into place. A
//!     compromised/MITM'd release host (or a forged `.sig`) can therefore never
//!     get a trojan AppImage installed — verification fails closed and the live
//!     install is left untouched. This is the property that makes auto-update
//!     over an untrusted network safe for external machines.
//!   * Downloads to a sibling `.part` file, validates signature + ELF magic,
//!     and only then atomically renames over the live AppImage. A
//!     partial/garbage/unsigned download can never corrupt a working install.
//!   * Every network/IO error degrades to a calm result; nothing panics.
//!
//! Freshness is keyed off the release asset's `id:created_at:size`. The release
//! script deletes+recreates the release and re-uploads assets on every publish,
//! so the asset id (a DB autoincrement) always changes for a new build — this
//! catches re-releases under the same tag, which a version/tag compare misses.
//! An update is only ever surfaced as `available` when the release also carries
//! the matching `<asset>.sig`, so a build published without a signature never
//! produces a doomed "update available" the client could not apply.

use base64::Engine;
use serde::Serialize;

const REPO: &str = "NetworkBound/cortex";

/// Baked-in ed25519 public key (base64 of the raw 32 bytes) used to verify the
/// signature on every downloaded AppImage. The matching private key lives only
/// on the release machine (`~/.cortex/update-signing-key.pem`, never committed)
/// and signs each artifact in `scripts/gitea-publish-release.sh`. Public keys
/// are safe to ship; a key rotation can override this at runtime via
/// `CORTEX_UPDATE_PUBKEY` / `infra.json` `update_pubkey` without a rebuild.
const DEFAULT_UPDATE_PUBKEY: &str = "eX81qpl/U3i/lgrgVGgisAbhGtRFWMZsQvvzYLRqwJo=";

/// Resolve the active update-signing public key: runtime override
/// (`CORTEX_UPDATE_PUBKEY` → `infra.json`) → the baked-in default. Always
/// returns a key, so signature verification is mandatory and never silently
/// disabled.
fn update_pubkey() -> String {
    crate::infra_config::update_pubkey().unwrap_or_else(|| DEFAULT_UPDATE_PUBKEY.to_string())
}

/// Verify a detached ed25519 signature (`sig_b64`, base64 of the raw 64-byte
/// signature) over `message`, against `pubkey_b64` (base64 of the raw 32-byte
/// public key). `Ok(())` only on a cryptographically valid signature; every
/// other case (bad base64, wrong length, forged/altered bytes) is an `Err`.
/// Pure + side-effect-free, so the security-critical path is unit-testable.
fn verify_signature(pubkey_b64: &str, message: &[u8], sig_b64: &str) -> Result<(), String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let pubkey_bytes = engine
        .decode(pubkey_b64.trim())
        .map_err(|_| "update public key is not valid base64".to_string())?;
    let sig_bytes = engine
        .decode(sig_b64.trim())
        .map_err(|_| "update signature is not valid base64".to_string())?;
    let key =
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pubkey_bytes.as_slice());
    key.verify(message, sig_bytes.as_slice())
        .map_err(|_| "update signature verification failed".to_string())
}

/// Gitea host (scheme + host + port) the updater pulls releases from.
/// Resolved via `CORTEX_UPDATE_GITEA_HOST` env → `~/.cortex/infra.json`
/// (`update_gitea_host`) → `None`. No baked-in default ships in the binary;
/// when unconfigured the self-updater is quietly idle (`available: false`,
/// no network I/O) and `apply` fails closed with a humanized message.
fn gitea_host() -> Option<String> {
    crate::infra_config::update_gitea_host()
}

#[derive(Debug, Serialize, Default)]
pub struct ReleaseUpdate {
    /// True only when we're a replaceable Linux AppImage.
    pub supported: bool,
    /// True when the latest release asset differs from what's installed.
    pub available: bool,
    pub current_key: Option<String>,
    pub latest_key: Option<String>,
    pub tag: Option<String>,
    pub asset_name: Option<String>,
    pub download_url: Option<String>,
}

/// Absolute path of the running AppImage, if we are one. `None` everywhere
/// else (Windows, .deb install, `cargo run`), which disables self-update.
#[cfg(target_os = "linux")]
fn appimage_path() -> Option<String> {
    std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty())
}

/// Sidecar file that records the asset key of the currently-installed build.
/// Shared with `scripts/cortex-autoupdate.sh` so the two mechanisms agree.
#[cfg(target_os = "linux")]
fn state_path(appimage: &str) -> String {
    format!("{appimage}.update-state")
}

/// Prefix every legitimate AppImage download URL must start with. Anything
/// outside our own Gitea release-download path is rejected before we fetch it,
/// so a malicious/compromised caller cannot point the updater at an arbitrary
/// host (SSRF / supply a trojan AppImage). Fails closed.
fn release_download_prefix(gitea_host: &str) -> String {
    format!("{gitea_host}/{REPO}/releases/download/")
}

/// True only for URLs that target our configured Gitea release-download path.
/// Rejects scheme/host/path mismatches and embedded credentials or `@` tricks.
/// `None` host (self-update unconfigured) rejects everything — fail closed.
fn is_trusted_download_url(url: &str, gitea_host: Option<&str>) -> bool {
    let Some(host) = gitea_host else { return false };
    let prefix = release_download_prefix(host);
    // Must be an exact prefix match on the canonical release-download path.
    // (The host already pins scheme + host + port.) A bare prefix with no
    // asset path after it is not a real asset, so require more characters.
    url.starts_with(&prefix) && url.len() > prefix.len() && !url.contains("..")
}

/// Parsed view of the newest release's AppImage asset.
struct LatestAsset {
    tag: String,
    name: String,
    key: String,
    url: String,
    /// Download URL of the detached `<name>.sig` signature, when the release
    /// carries one. `None` → the build is unsigned, so it is never offered as
    /// an applicable update (a signed build is required to apply anything).
    sig_url: Option<String>,
}

/// Build the AppImage asset view from the newest release JSON, or `None`.
fn parse_latest(body: &str, gitea_host: &str) -> Option<LatestAsset> {
    let arr: serde_json::Value = serde_json::from_str(body).ok()?;
    let rel = arr.as_array()?.first()?;
    let tag = rel.get("tag_name")?.as_str()?.to_string();
    let assets = rel.get("assets")?.as_array()?;

    // Find the AppImage asset first; the `.sig` asset is matched by name below.
    let appimage = assets.iter().find(|a| {
        a.get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|n| n.ends_with("amd64.AppImage"))
    })?;
    let name = appimage.get("name").and_then(|v| v.as_str())?.to_string();
    let id = appimage.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let created = appimage
        .get("created_at")
        .and_then(|v| v.as_str())
        .or_else(|| appimage.get("updated_at").and_then(|v| v.as_str()))
        .unwrap_or("");
    let size = appimage.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
    let key = format!("{id}:{created}:{size}");
    let url = format!("{gitea_host}/{REPO}/releases/download/{tag}/{name}");

    let sig_name = format!("{name}.sig");
    let sig_present = assets.iter().any(|a| {
        a.get("name").and_then(|v| v.as_str()) == Some(sig_name.as_str())
    });
    let sig_url = sig_present
        .then(|| format!("{gitea_host}/{REPO}/releases/download/{tag}/{sig_name}"));

    Some(LatestAsset { tag, name, key, url, sig_url })
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn check_release_update() -> Result<ReleaseUpdate, String> {
    let appimage = match appimage_path() {
        Some(p) => p,
        None => return Ok(ReleaseUpdate::default()), // supported: false
    };

    // No release host configured → quietly idle: supported but never
    // "available", and crucially no network I/O / no error spam.
    let Some(host) = gitea_host() else {
        tracing::debug!("selfupdate: no update host configured; skipping check");
        return Ok(ReleaseUpdate { supported: true, ..Default::default() });
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let api = format!("{host}/api/v1/repos/{REPO}/releases?limit=1");
    let body = match client.get(&api).send().await {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        Ok(r) => {
            tracing::info!("selfupdate: releases list returned {}", r.status());
            return Ok(ReleaseUpdate { supported: true, ..Default::default() });
        }
        Err(e) => {
            tracing::info!("selfupdate: fetch failed: {e}");
            return Ok(ReleaseUpdate { supported: true, ..Default::default() });
        }
    };

    let latest = match parse_latest(&body, &host) {
        Some(v) => v,
        None => return Ok(ReleaseUpdate { supported: true, ..Default::default() }),
    };

    let current = std::fs::read_to_string(state_path(&appimage))
        .ok()
        .map(|s| s.trim().to_string());

    // Only offer an update when (a) the asset key changed AND (b) the release
    // ships a `.sig` we will be able to verify. An unsigned build is treated
    // as "nothing to apply" rather than a doomed available→reject loop.
    let key_changed = current.as_deref() != Some(latest.key.as_str());
    if key_changed && latest.sig_url.is_none() {
        tracing::info!("selfupdate: newer release {} has no .sig asset; skipping (signature required)", latest.tag);
    }
    let available = key_changed && latest.sig_url.is_some();

    Ok(ReleaseUpdate {
        supported: true,
        available,
        current_key: current,
        latest_key: Some(latest.key),
        tag: Some(latest.tag),
        asset_name: Some(latest.name),
        download_url: Some(latest.url),
    })
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn apply_release_update(download_url: String, asset_key: String) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;

    let appimage = appimage_path().ok_or_else(|| "not running as an AppImage".to_string())?;
    let part = format!("{appimage}.part");

    // Fail closed: only ever download from our own Gitea release-download path.
    // The caller-supplied URL is otherwise untrusted and could point anywhere.
    // An unconfigured host rejects everything with a humanized message.
    let host = gitea_host();
    if host.is_none() {
        return Err(
            "self-update is not configured — set update_gitea_host in ~/.cortex/infra.json (or CORTEX_UPDATE_GITEA_HOST)"
                .to_string(),
        );
    }
    if !is_trusted_download_url(&download_url, host.as_deref()) {
        tracing::warn!("selfupdate: refused untrusted download_url");
        return Err("refused: download_url is not a trusted Gitea release URL".to_string());
    }

    // The detached signature lives next to the asset as `<asset>.sig`. It must
    // also be on our trusted release path (no SSRF via a forged sig URL).
    let sig_url = format!("{download_url}.sig");
    if !is_trusted_download_url(&sig_url, host.as_deref()) {
        return Err("refused: signature URL is not a trusted Gitea release URL".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    // Fetch the signature first — a release without one cannot be applied.
    let sig_resp = client
        .get(&sig_url)
        .send()
        .await
        .map_err(|e| format!("signature download failed: {e}"))?;
    if !sig_resp.status().is_success() {
        return Err(format!(
            "update is unsigned (signature fetch returned {}); refusing to apply",
            sig_resp.status()
        ));
    }
    let sig_b64 = sig_resp
        .text()
        .await
        .map_err(|e| format!("signature read failed: {e}"))?;

    let resp = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download returned {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read failed: {e}"))?;

    // SECURITY GATE: verify the ed25519 signature over the downloaded bytes
    // against the baked-in (or configured) public key BEFORE touching disk.
    // A forged/tampered/unsigned artifact fails here and never reaches the
    // live install path.
    verify_signature(&update_pubkey(), &bytes, &sig_b64)
        .map_err(|e| format!("refusing update: {e}"))?;

    // Guard: AppImages are ELF binaries. Refuse anything else (e.g. an HTML
    // error page) so we never clobber a working install.
    if bytes.len() < 4 || &bytes[..4] != b"\x7fELF" {
        return Err("downloaded file is not an ELF/AppImage".to_string());
    }

    std::fs::write(&part, &bytes).map_err(|e| format!("write failed: {e}"))?;
    let perm = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&part, perm).map_err(|e| format!("chmod failed: {e}"))?;
    std::fs::rename(&part, &appimage).map_err(|e| format!("swap failed: {e}"))?;
    let _ = std::fs::write(state_path(&appimage), asset_key);

    Ok("applied".to_string())
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub fn relaunch_app(app: tauri::AppHandle) {
    app.restart();
}

// ---- Non-Linux stubs: self-update is a no-op (compiles for the Windows build).

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn check_release_update() -> Result<ReleaseUpdate, String> {
    Ok(ReleaseUpdate::default())
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn apply_release_update(_download_url: String, _asset_key: String) -> Result<String, String> {
    Err("self-update is only supported on the Linux AppImage build".to_string())
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub fn relaunch_app(_app: tauri::AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Documentation-only example host — no real deployment value ships in
    /// source. The runtime host comes from env / ~/.cortex/infra.json.
    const HOST: &str = "http://git.example.com:3000";

    // A signed release: the AppImage plus its detached `<name>.sig`.
    const SAMPLE: &str = r#"[
      {"tag_name":"v0.0.1-20260531","assets":[
        {"name":"Cortex_0.0.1_amd64.deb","id":31,"created_at":"2026-05-30T23:44:00-09:00","size":14000000},
        {"name":"Cortex_0.0.1_amd64.AppImage","id":33,"created_at":"2026-05-30T23:45:10-09:00","size":92043768},
        {"name":"Cortex_0.0.1_amd64.AppImage.sig","id":34,"created_at":"2026-05-30T23:45:20-09:00","size":89}
      ]}
    ]"#;

    // An UNSIGNED release: AppImage but no `.sig` asset.
    const SAMPLE_UNSIGNED: &str = r#"[
      {"tag_name":"v0.0.1-20260531","assets":[
        {"name":"Cortex_0.0.1_amd64.AppImage","id":33,"created_at":"2026-05-30T23:45:10-09:00","size":92043768}
      ]}
    ]"#;

    // ---- Committed ed25519 test vector (generated offline with openssl, the
    // same tool the publish script uses). Lets the security-critical verifier
    // be exercised deterministically and offline.
    const TV_PUBKEY: &str = "Hp11bUYk59lvn5g12A484lyJQZGIHA+fh9GKic+froA=";
    const TV_MESSAGE: &[u8] = b"cortex-update-test-vector-v1";
    const TV_SIG: &str =
        "Z0nMm7SanA5iv0ufrffU3kVkLgMNish2ou0BLBV7kVJowASjBiTW1V6/ePKYTL94VhT98snI0h/BVREdMm+ICQ==";

    #[test]
    fn picks_appimage_asset_and_builds_key_url_and_sig() {
        let l = parse_latest(SAMPLE, HOST).expect("parse");
        assert_eq!(l.tag, "v0.0.1-20260531");
        assert_eq!(l.name, "Cortex_0.0.1_amd64.AppImage");
        assert_eq!(l.key, "33:2026-05-30T23:45:10-09:00:92043768");
        assert_eq!(
            l.url,
            "http://git.example.com:3000/NetworkBound/cortex/releases/download/v0.0.1-20260531/Cortex_0.0.1_amd64.AppImage"
        );
        assert_eq!(
            l.sig_url.as_deref(),
            Some("http://git.example.com:3000/NetworkBound/cortex/releases/download/v0.0.1-20260531/Cortex_0.0.1_amd64.AppImage.sig")
        );
    }

    #[test]
    fn unsigned_release_has_no_sig_url() {
        let l = parse_latest(SAMPLE_UNSIGNED, HOST).expect("parse");
        assert_eq!(l.name, "Cortex_0.0.1_amd64.AppImage");
        assert!(l.sig_url.is_none(), "unsigned release must not yield a sig url");
    }

    #[test]
    fn valid_signature_verifies() {
        assert!(verify_signature(TV_PUBKEY, TV_MESSAGE, TV_SIG).is_ok());
    }

    #[test]
    fn tampered_message_fails_verification() {
        assert!(verify_signature(TV_PUBKEY, b"cortex-update-test-vector-v2", TV_SIG).is_err());
        assert!(verify_signature(TV_PUBKEY, b"", TV_SIG).is_err());
    }

    #[test]
    fn wrong_key_or_garbage_sig_fails_verification() {
        // Right message, wrong (production) key -> reject.
        assert!(verify_signature(DEFAULT_UPDATE_PUBKEY, TV_MESSAGE, TV_SIG).is_err());
        // Malformed base64 / wrong-length inputs all fail closed (never panic).
        assert!(verify_signature("not base64!!", TV_MESSAGE, TV_SIG).is_err());
        assert!(verify_signature(TV_PUBKEY, TV_MESSAGE, "not base64!!").is_err());
        assert!(verify_signature(TV_PUBKEY, TV_MESSAGE, "AAAA").is_err());
        assert!(verify_signature("AAAA", TV_MESSAGE, TV_SIG).is_err());
    }

    #[test]
    fn baked_pubkey_is_valid_base64_32_bytes() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(DEFAULT_UPDATE_PUBKEY)
            .expect("baked pubkey must be valid base64");
        assert_eq!(raw.len(), 32, "ed25519 public key must be 32 bytes");
    }

    #[test]
    fn no_appimage_asset_returns_none() {
        let body = r#"[{"tag_name":"v1","assets":[{"name":"notes.txt","id":1,"size":3}]}]"#;
        assert!(parse_latest(body, HOST).is_none());
    }

    #[test]
    fn empty_or_garbage_returns_none() {
        assert!(parse_latest("[]", HOST).is_none());
        assert!(parse_latest("not json", HOST).is_none());
        assert!(parse_latest("{}", HOST).is_none());
    }

    #[test]
    fn trusts_only_configured_gitea_release_urls() {
        let good = parse_latest(SAMPLE, HOST).unwrap().url;
        assert!(is_trusted_download_url(&good, Some(HOST)));
        // The derived `<asset>.sig` URL is also on the trusted release path.
        assert!(is_trusted_download_url(&format!("{good}.sig"), Some(HOST)));

        // Wrong host / scheme / path, traversal, bare prefix -> all rejected.
        assert!(!is_trusted_download_url("https://evil.example/x.AppImage", Some(HOST)));
        assert!(!is_trusted_download_url(
            "http://git.example.com:3000/NetworkBound/cortex/releases/download/",
            Some(HOST)
        ));
        assert!(!is_trusted_download_url(
            "http://git.example.com:3000/Other/repo/releases/download/v1/x.AppImage",
            Some(HOST)
        ));
        assert!(!is_trusted_download_url(
            "http://git.example.com:3000/NetworkBound/cortex/releases/download/../../etc/passwd",
            Some(HOST)
        ));
        assert!(!is_trusted_download_url(
            "http://evil@git.example.com:3000/NetworkBound/cortex/releases/download/v1/x.AppImage",
            Some(HOST)
        ));
    }

    #[test]
    fn unconfigured_host_rejects_every_url() {
        // Fail closed: with no update host configured nothing is trusted.
        let good = parse_latest(SAMPLE, HOST).unwrap().url;
        assert!(!is_trusted_download_url(&good, None));
        assert!(!is_trusted_download_url("https://anything.example/x.AppImage", None));
    }

    #[test]
    fn key_changes_when_asset_reuploaded() {
        // Same tag/name/size but a new upload id -> different key -> update seen.
        let reup = SAMPLE.replace("\"id\":33", "\"id\":40");
        let k1 = parse_latest(SAMPLE, HOST).unwrap().key;
        let k2 = parse_latest(&reup, HOST).unwrap().key;
        assert_ne!(k1, k2);
    }
}
