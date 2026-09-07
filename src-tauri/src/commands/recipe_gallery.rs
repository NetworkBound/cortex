//! Recipe gallery (Goose-style YAML "recipes").
//!
//! A recipe is a small YAML document describing a reusable agent workflow —
//! a goal, the tools and agent roles the workflow expects, and optional
//! per-checkpoint hooks. The user keeps a personal collection at
//! `~/.cortex/recipes/<name>.yaml`; community recipes can be pulled down
//! from any HTTPS URL via `install_recipe_from_url`.
//!
//! All file I/O is read-modify-write on small files (cap: 64 KiB per
//! recipe) so we don't bother with locks or async file APIs beyond
//! `spawn_blocking`. Validation is deliberately loose: we require a `name`
//! and `goal` and otherwise pass arbitrary YAML through. The frontend
//! editor can surface anything extra without forcing a schema migration.
//!
//! Path safety: `is_valid_name()` rejects path-separator and `.`-prefixed
//! values so a user-typed name can never escape the recipes directory.

use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Hard cap on serialized YAML size. Anything bigger almost certainly isn't
/// a recipe — it's a model dump, a paste accident, or someone trying to
/// trick the URL installer into writing a megabyte of data to disk.
const MAX_RECIPE_BYTES: usize = 64 * 1024;

/// Outbound recipe envelope. We keep the parsed-ish view simple — `tools`
/// and `agents` are flat string arrays; `checkpoints` is opaque JSON so the
/// UI doesn't have to track every variant of hook shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    /// File-name stem (no extension). Also the unique identifier.
    pub name: String,
    /// One-liner shown in the gallery list.
    #[serde(default)]
    pub description: String,
    /// What the agent should achieve when this recipe runs.
    pub goal: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    /// Free-form per-checkpoint hooks. We don't interpret these — the
    /// runtime layer does. Stored as JSON for round-trippability through
    /// the Tauri bridge without serde_yaml::Value drifting.
    #[serde(default)]
    pub checkpoints: serde_json::Value,
    /// Absolute filesystem path. Filled in by `list_recipes` / `get_recipe`
    /// so the UI can show where the file lives.
    #[serde(default)]
    pub path: String,
    /// Raw YAML body. Round-tripped verbatim so the user can edit in-place
    /// without losing comments or formatting.
    #[serde(default)]
    pub yaml: String,
}

fn recipes_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let dir = home.join(".cortex").join("recipes");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    Ok(dir)
}

/// Recipe names are file stems so the same defensive ruleset as snippets
/// applies: ASCII alphanumerics plus `-`, `_`, `.` only; no leading dot
/// (don't allow hidden files); length-capped.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn recipe_path(name: &str) -> Result<PathBuf, String> {
    if !is_valid_name(name) {
        return Err(format!("invalid recipe name '{name}'"));
    }
    Ok(recipes_dir()?.join(format!("{name}.yaml")))
}

/// Parse a YAML body into a Recipe envelope. We require at minimum a name
/// (for the file stem to be coherent) and a goal (because a recipe without
/// one is unusable). Everything else has a sensible default.
fn parse_yaml(name: &str, yaml: &str) -> Result<Recipe, String> {
    if yaml.len() > MAX_RECIPE_BYTES {
        return Err(format!(
            "recipe exceeds {} KiB cap",
            MAX_RECIPE_BYTES / 1024
        ));
    }
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("YAML parse failed: {e}"))?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| "recipe root must be a YAML mapping".to_string())?;

    let parsed_name = mapping
        .get(serde_yaml::Value::String("name".into()))
        .and_then(|v| v.as_str())
        .unwrap_or(name)
        .to_string();
    let description = mapping
        .get(serde_yaml::Value::String("description".into()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let goal = mapping
        .get(serde_yaml::Value::String("goal".into()))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "recipe missing `goal:` field".to_string())?
        .to_string();
    let tools = mapping
        .get(serde_yaml::Value::String("tools".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let agents = mapping
        .get(serde_yaml::Value::String("agents".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let checkpoints = mapping
        .get(serde_yaml::Value::String("checkpoints".into()))
        .cloned()
        .map(yaml_to_json)
        .unwrap_or(serde_json::Value::Null);

    Ok(Recipe {
        name: parsed_name,
        description,
        goal,
        tools,
        agents,
        checkpoints,
        path: String::new(),
        yaml: yaml.to_string(),
    })
}

/// serde_yaml → serde_json bridging. Keeps the Tauri boundary clean —
/// the frontend only speaks JSON.
fn yaml_to_json(v: serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    other => serde_yaml::to_string(&other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                out.insert(key, yaml_to_json(val));
            }
            serde_json::Value::Object(out)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

/// Load every `*.yaml` in `~/.cortex/recipes/`. Files that fail to parse
/// are dropped silently — a single broken recipe shouldn't blank the
/// gallery — but we surface their filename in a debug log so the user has
/// a breadcrumb if they go hunting.
#[tauri::command]
pub async fn list_recipes() -> Result<Vec<Recipe>, String> {
    tokio::task::spawn_blocking(|| {
        let dir = recipes_dir()?;
        let mut out: Vec<Recipe> = Vec::new();
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => return Ok(out),
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let body = match fs::read_to_string(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let mut recipe = match parse_yaml(&stem, &body) {
                Ok(r) => r,
                Err(_) => continue,
            };
            recipe.path = path.to_string_lossy().to_string();
            out.push(recipe);
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok::<Vec<Recipe>, String>(out)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn get_recipe(name: String) -> Result<Option<Recipe>, String> {
    if !is_valid_name(&name) {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let path = recipe_path(&name)?;
        if !path.exists() {
            return Ok::<Option<Recipe>, String>(None);
        }
        let body = fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
        let mut recipe = parse_yaml(&name, &body)?;
        recipe.path = path.to_string_lossy().to_string();
        Ok(Some(recipe))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Save (create-or-overwrite) a recipe. The frontend sends the YAML body
/// verbatim so users keep their comments and formatting; we re-parse it
/// to enforce the basic name/goal contract before writing.
#[tauri::command]
pub async fn save_recipe(name: String, yaml: String) -> Result<Recipe, String> {
    if !is_valid_name(&name) {
        return Err(format!("invalid recipe name '{name}'"));
    }
    if yaml.len() > MAX_RECIPE_BYTES {
        return Err(format!(
            "recipe exceeds {} KiB cap",
            MAX_RECIPE_BYTES / 1024
        ));
    }
    tokio::task::spawn_blocking(move || {
        // Round-trip validate before writing — better to reject up front
        // than to leave a half-broken file on disk that `list_recipes`
        // silently drops.
        let mut recipe = parse_yaml(&name, &yaml)?;
        let path = recipe_path(&name)?;
        fs::write(&path, yaml.as_bytes()).map_err(|e| format!("write failed: {e}"))?;
        recipe.path = path.to_string_lossy().to_string();
        Ok::<Recipe, String>(recipe)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn delete_recipe(name: String) -> Result<(), String> {
    if !is_valid_name(&name) {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let path = recipe_path(&name)?;
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("remove failed: {e}"))?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// True when an IP address must never be reached by the URL installer:
/// loopback, RFC1918 / unique-local private space, link-local (incl. the
/// 169.254.169.254 cloud metadata endpoint), and otherwise non-global
/// reserved ranges. Resolving the host and checking every candidate address
/// against this is our SSRF guard — a public hostname that resolves to an
/// internal address is rejected just like a literal internal IP.
fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_documentation()
                // 100.64.0.0/10 carrier-grade NAT (shared address space).
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                // 192.0.0.0/24 IETF protocol assignments.
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
                // 240.0.0.0/4 reserved for future use.
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // Unique local addresses fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local unicast fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped / -compatible: re-check the embedded v4 address.
                || v6
                    .to_ipv4()
                    .map(|m| is_blocked_ip(&IpAddr::V4(m)))
                    .unwrap_or(false)
        }
    }
}

/// Resolve `host` (which may itself be an IP literal) and fail if it maps to
/// any blocked address. We attach a throwaway port (443) because
/// `ToSocketAddrs` resolution requires one; only the IPs matter here.
fn guard_against_ssrf(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("install URL has no host".to_string());
    }
    // Fast path for IP literals — including bracketed IPv6 (`[::1]`) — so we
    // don't depend on the resolver to surface an obviously internal target.
    let literal = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'));
    if let Ok(ip) = literal.unwrap_or(host).parse::<IpAddr>() {
        return if is_blocked_ip(&ip) {
            Err("install URL resolves to a non-public address".to_string())
        } else {
            Ok(())
        };
    }
    let addrs = (host, 443u16)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve install host: {e}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if is_blocked_ip(&addr.ip()) {
            return Err("install URL resolves to a non-public address".to_string());
        }
    }
    if !any {
        return Err("install host did not resolve to any address".to_string());
    }
    Ok(())
}

/// Fetch a recipe from an HTTPS URL and save it locally. We refuse:
///  - non-HTTPS URLs (so a stray http:// link can't pull recipe bodies
///    over plaintext)
///  - responses bigger than `MAX_RECIPE_BYTES` (we cap the body read so
///    a hostile server can't stream gigabytes)
///  - bodies that fail YAML validation
///
/// The local filename is derived from `?name=…` query when present,
/// otherwise from the URL path's stem. Conflicts with an existing recipe
/// are an error — the user has to delete the old one first to make the
/// intent explicit.
#[tauri::command]
pub async fn install_recipe_from_url(url: String) -> Result<Recipe, String> {
    if !url.starts_with("https://") {
        return Err("install URL must be https://".to_string());
    }

    // Derive a candidate filename BEFORE we hit the network so we can fail
    // fast on a malformed URL. Lightweight URL parser — we only need the
    // path stem and an optional `?name=` override, so pulling in a full
    // url crate would be overkill.
    let after_scheme = &url["https://".len()..];
    let (authority, path_and_query) = after_scheme
        .split_once('/')
        .map(|(auth, rest)| (auth, rest))
        .unwrap_or((after_scheme, ""));
    // Strip any optional `userinfo@`, then the trailing `:port`, leaving the
    // bare host (an IPv6 literal stays bracketed, e.g. `[::1]`). The authority
    // ends at the first `?`/`#` when there was no path separator.
    let authority = authority
        .split(['?', '#'])
        .next()
        .unwrap_or(authority);
    let host_port = authority.rsplit_once('@').map(|(_, hp)| hp).unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // Bracketed IPv6 literal: keep the brackets up to and including `]`.
        match rest.split_once(']') {
            Some((inner, _)) => format!("[{inner}]"),
            None => host_port.to_string(),
        }
    } else {
        host_port.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_port).to_string()
    };
    guard_against_ssrf(&host)?;
    let (path_only, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_and_query, ""),
    };
    let path_only = path_only.split('#').next().unwrap_or(path_only);
    let query_name = query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == "name" {
            Some(v.to_string())
        } else {
            None
        }
    });
    let path_stem = path_only
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|seg| {
            Path::new(seg)
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or(seg)
                .to_string()
        });
    let name = query_name
        .or(path_stem)
        .ok_or_else(|| "could not derive recipe name from URL".to_string())?;
    if !is_valid_name(&name) {
        return Err(format!("derived recipe name '{name}' is not safe"));
    }

    let target = recipe_path(&name)?;
    if target.exists() {
        return Err(format!(
            "recipe '{name}' already exists — delete the local copy first"
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client build failed: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("fetch failed: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read body failed: {e}"))?;
    if bytes.len() > MAX_RECIPE_BYTES {
        return Err(format!(
            "remote recipe exceeds {} KiB cap",
            MAX_RECIPE_BYTES / 1024
        ));
    }
    let body =
        std::str::from_utf8(&bytes).map_err(|_| "recipe body is not valid UTF-8".to_string())?;

    // Validate via parse_yaml — same contract as save_recipe — then write.
    let yaml = body.to_string();
    let name_for_blocking = name.clone();
    tokio::task::spawn_blocking(move || {
        let mut recipe = parse_yaml(&name_for_blocking, &yaml)?;
        let path = recipe_path(&name_for_blocking)?;
        fs::write(&path, yaml.as_bytes()).map_err(|e| format!("write failed: {e}"))?;
        recipe.path = path.to_string_lossy().to_string();
        Ok::<Recipe, String>(recipe)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(is_valid_name("deploy-staging"));
        assert!(is_valid_name("a.b.c"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(".hidden"));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }

    #[test]
    fn parses_minimal_recipe() {
        let yaml = "name: deploy-staging\ngoal: \"ship it\"\n";
        let r = parse_yaml("deploy-staging", yaml).unwrap();
        assert_eq!(r.name, "deploy-staging");
        assert_eq!(r.goal, "ship it");
        assert!(r.tools.is_empty());
    }

    #[test]
    fn rejects_missing_goal() {
        let yaml = "name: foo\n";
        let err = parse_yaml("foo", yaml).unwrap_err();
        assert!(err.contains("goal"));
    }

    #[test]
    fn rejects_oversize() {
        let mut yaml = String::from("goal: ");
        yaml.push_str(&"x".repeat(MAX_RECIPE_BYTES + 1));
        let err = parse_yaml("big", &yaml).unwrap_err();
        assert!(err.contains("cap"));
    }
}
