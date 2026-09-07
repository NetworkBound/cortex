//! Diagnostics export — one-click bundle a stranger can attach to a bug
//! report without leaking anything private.
//!
//! Produces `~/.cortex/diagnostics-<ts>.tar.gz` containing:
//!   - `meta.json`         — app version, build variant, runtime mode, OS/arch
//!   - `config.json`       — the live Config snapshot, REDACTED
//!   - `crash-log.json`    — recent rows from the SQLite `crash_log` table
//!                            (written by the panic hook), REDACTED
//!   - `sessions.json`     — recent session *metadata* only (ids, counts,
//!                            timestamps) — NEVER message contents
//!   - `cortex-config/*`   — an explicit ALLOWLIST of `~/.cortex` json files,
//!                            REDACTED. `keys.enc` and anything not listed is
//!                            never touched.
//!
//! Every byte that lands in the archive flows through [`redact_diagnostics`],
//! which layers private-IP and home-path scrubbing on top of the shared
//! secret redactor in `crate::redact`. The bundle builder is a pure function
//! so the "no secrets in any file" property is unit-testable.

use crate::app_state::AppState;
use crate::observability::crash;
use crate::observability::tracing_store::{AuditRow, TracingStore};
use crate::redact::redact_text;
use flate2::write::GzEncoder;
use flate2::Compression;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

const IP_MASK: &str = "[PRIVATE-IP]";

/// RFC1918 ranges (10/8, 172.16/12, 192.168/16) plus the Tailscale CGNAT
/// range (100.64.0.0/10), with an optional `:port` so URLs collapse cleanly.
static PRIVATE_IP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"\b(?:",
        r"10\.\d{1,3}\.\d{1,3}\.\d{1,3}",
        r"|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}",
        r"|192\.168\.\d{1,3}\.\d{1,3}",
        r"|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3}",
        r")(?::\d{1,5})?\b",
    ))
    .unwrap()
});

/// Home-directory prefixes (Linux / macOS / Windows). Only the prefix +
/// username collapses to `~`; the path tail stays readable so a bug report
/// still shows *which* file was involved.
static HOME_PATH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:/home/|/Users/|[A-Z]:[\\/]Users[\\/])[A-Za-z0-9._-]+").unwrap());

/// JSON-aware sensitive assignments. The shared `redact::redact_text`
/// ASSIGN_RE needs a word boundary before the key word and no quote between
/// key and separator, so `"gateway_api_key": "v"` slips through both. This
/// pattern catches any identifier *ending* in a sensitive suffix, in plain
/// (`x=y`) or JSON (`"x": "y"`) form. Over-matching (e.g. `monkey=...`) is an
/// acceptable cost: for diagnostics, leaking nothing beats reading nicely.
static SENSITIVE_ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)([A-Za-z0-9_.-]*(?:password|passwd|secret|token|credential|api[_-]?key|apikey|key)"?\s*[:=]\s*"?)([^\s"',;]+)"#,
    )
    .unwrap()
});

/// The diagnostics redactor: shared secret scrubbing (keys, tokens, JWTs,
/// PEM blocks, `key=value` assignments) THEN JSON-style sensitive
/// assignments THEN private/tailnet IPs THEN home paths. Everything written
/// into the bundle must pass through here.
pub fn redact_diagnostics(input: &str) -> String {
    let s = crate::redact::redact_text(input);
    let s = SENSITIVE_ASSIGN_RE.replace_all(&s, "${1}[REDACTED]");
    let s = PRIVATE_IP_RE.replace_all(&s, IP_MASK);
    HOME_PATH_RE.replace_all(&s, "~").into_owned()
}

/// Session *metadata* row — deliberately content-free.
#[derive(Debug, Serialize)]
struct SessionMeta {
    session_id: String,
    message_count: i64,
    first_ts: i64,
    last_ts: i64,
    project_root: Option<String>,
}

fn recent_session_meta(
    conn: &Mutex<Connection>,
    limit: usize,
) -> anyhow::Result<Vec<SessionMeta>> {
    let guard = conn.lock();
    let mut stmt = guard.prepare(
        "SELECT session_id, COUNT(*), MIN(ts), MAX(ts), MAX(COALESCE(project_root, ''))
         FROM messages GROUP BY session_id ORDER BY MAX(ts) DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        let root: String = r.get(4)?;
        Ok(SessionMeta {
            session_id: r.get(0)?,
            message_count: r.get(1)?,
            first_ts: r.get(2)?,
            last_ts: r.get(3)?,
            project_root: if root.is_empty() { None } else { Some(root) },
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Plain-data snapshot of the live `Config`, so the bundle builder stays a
/// pure function over owned values (no lock guards, no `State`).
#[derive(Debug, Clone, Serialize)]
pub struct ConfigSnapshot {
    pub gateway_base_url: String,
    pub gateway_model: String,
    pub default_project_root: Option<String>,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub obsidian_vault: Option<String>,
    pub sandbox_tier: Option<String>,
    pub reasoning_effort: Option<String>,
    pub active_profile: Option<String>,
    pub git_server_url: Option<String>,
    pub git_server_cloned_path: Option<String>,
    pub runtime_mode: String,
}

impl ConfigSnapshot {
    fn from_config(cfg: &crate::app_state::Config) -> Self {
        let p = |o: &Option<PathBuf>| o.as_ref().map(|p| p.to_string_lossy().into_owned());
        Self {
            gateway_base_url: cfg.gateway_base_url.clone(),
            gateway_model: cfg.gateway_model.clone(),
            default_project_root: p(&cfg.default_project_root),
            ollama_base_url: cfg.ollama_base_url.clone(),
            ollama_model: cfg.ollama_model.clone(),
            obsidian_vault: p(&cfg.obsidian_vault),
            sandbox_tier: cfg.sandbox_tier.clone(),
            reasoning_effort: cfg.reasoning_effort.clone(),
            active_profile: cfg.active_profile.clone(),
            git_server_url: cfg.git_server_url.clone(),
            git_server_cloned_path: p(&cfg.git_server_cloned_path),
            runtime_mode: cfg.runtime_mode.clone(),
        }
    }
}

/// `~/.cortex` files that are safe to include AFTER redaction. This is an
/// allowlist on purpose: `keys.enc` (the encrypted key vault) and any future
/// secret-bearing file must never ride along by accident.
const CORTEX_CONFIG_ALLOWLIST: &[&str] = &["last-project.json", "git-config.json"];

/// Build every file of the bundle as `(name, redacted_content)` pairs.
/// Pure over its inputs — the command gathers, this assembles + redacts.
fn build_bundle_files(
    config: &ConfigSnapshot,
    crashes_json: &str,
    sessions_json: &str,
    cortex_config_files: &[(String, String)],
) -> Vec<(String, String)> {
    let meta = serde_json::json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "build_variant": if cfg!(feature = "standalone") { "standalone" } else { "homelab" },
        "runtime_mode": config.runtime_mode,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "exported_at": chrono::Utc::now().to_rfc3339(),
    });
    let config_json =
        serde_json::to_string_pretty(config).unwrap_or_else(|_| "{}".to_string());

    let mut files: Vec<(String, String)> = vec![
        (
            "meta.json".to_string(),
            serde_json::to_string_pretty(&meta).unwrap_or_else(|_| "{}".to_string()),
        ),
        ("config.json".to_string(), config_json),
        ("crash-log.json".to_string(), crashes_json.to_string()),
        ("sessions.json".to_string(), sessions_json.to_string()),
    ];
    for (name, content) in cortex_config_files {
        files.push((format!("cortex-config/{name}"), content.clone()));
    }

    // The single redaction choke point: no file enters the archive raw.
    files
        .into_iter()
        .map(|(name, content)| (name, redact_diagnostics(&content)))
        .collect()
}

fn write_tar_gz(path: &PathBuf, files: &[(String, String)]) -> anyhow::Result<()> {
    let f = std::fs::File::create(path)?;
    let enc = GzEncoder::new(f, Compression::default());
    let mut tar = tar::Builder::new(enc);
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    for (name, content) in files {
        let bytes = content.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(now);
        header.set_cksum();
        tar.append_data(&mut header, format!("diagnostics/{name}"), bytes)?;
    }
    tar.into_inner()?.finish()?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsExport {
    /// Absolute path of the written archive (shown to the user, so NOT
    /// home-path-redacted — they need the real location to attach it).
    pub path: String,
    /// Names of the files inside the archive, for the UI summary.
    pub files: Vec<String>,
}

#[tauri::command]
pub async fn export_diagnostics(
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<DiagnosticsExport, String> {
    // Snapshot the config without holding the lock across anything.
    let config = {
        let cfg = state.config.read();
        ConfigSnapshot::from_config(&cfg)
    };

    // Crash log via the same connection the panic hook writes to.
    let conn = store.shared_connection();
    let crashes = crash::recent_crashes(&conn, 100).unwrap_or_default();
    let crashes_json =
        serde_json::to_string_pretty(&crashes).map_err(|e| e.to_string())?;

    // Recent session metadata — ids/counts/timestamps only, never contents.
    let sessions = recent_session_meta(&conn, 25).unwrap_or_default();
    let sessions_json =
        serde_json::to_string_pretty(&sessions).map_err(|e| e.to_string())?;
    drop(conn);

    // Allowlisted ~/.cortex config files (redacted later, with everything else).
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    let cortex_dir = home.join(".cortex");
    let mut cfg_files: Vec<(String, String)> = Vec::new();
    for name in CORTEX_CONFIG_ALLOWLIST {
        if let Ok(raw) = std::fs::read_to_string(cortex_dir.join(name)) {
            // Size cap: a config file should be tiny; never tar something huge.
            if raw.len() <= 64 * 1024 {
                cfg_files.push((name.to_string(), raw));
            }
        }
    }

    let files = build_bundle_files(&config, &crashes_json, &sessions_json, &cfg_files);

    std::fs::create_dir_all(&cortex_dir).map_err(|e| e.to_string())?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let out_path = cortex_dir.join(format!("diagnostics-{ts}.tar.gz"));
    write_tar_gz(&out_path, &files).map_err(|e| e.to_string())?;

    Ok(DiagnosticsExport {
        path: out_path.to_string_lossy().into_owned(),
        files: files.into_iter().map(|(n, _)| n).collect(),
    })
}

// ---------------------------------------------------------------------------
// Audit log export (Safe Mode / issue 004)
// ---------------------------------------------------------------------------

/// Quote a CSV field per RFC 4180: wrap in quotes, double inner quotes.
fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Render audit rows as `jsonl` or `csv`. Pure, and every free-text field
/// passes through the shared secret-redaction choke point (`redact_text`)
/// BEFORE serialization, so the export can never leak a credential that a
/// tool call happened to echo — the "no secrets in the export" property is
/// unit-tested below.
pub fn format_audit_rows(rows: &[AuditRow], format: &str) -> Result<String, String> {
    let redacted = rows.iter().map(|r| AuditRow {
        ts: r.ts,
        session_id: r.session_id.as_deref().map(redact_text),
        agent_id: r.agent_id.as_deref().map(redact_text),
        action: redact_text(&r.action),
        detail: r.detail.as_deref().map(redact_text),
    });
    match format.trim().to_ascii_lowercase().as_str() {
        "jsonl" => {
            let mut out = String::new();
            for row in redacted {
                let line = serde_json::to_string(&row).map_err(|e| e.to_string())?;
                out.push_str(&line);
                out.push('\n');
            }
            Ok(out)
        }
        "csv" => {
            let mut out = String::from("ts,session_id,agent_id,action,detail\n");
            for row in redacted {
                out.push_str(&format!(
                    "{},{},{},{},{}\n",
                    row.ts,
                    csv_field(row.session_id.as_deref().unwrap_or("")),
                    csv_field(row.agent_id.as_deref().unwrap_or("")),
                    csv_field(&row.action),
                    csv_field(row.detail.as_deref().unwrap_or("")),
                ));
            }
            Ok(out)
        }
        other => Err(format!("unknown export format '{other}' (expected \"jsonl\" or \"csv\")")),
    }
}

#[derive(Debug, Serialize)]
pub struct AuditLogExport {
    /// Absolute path of the written export file.
    pub path: String,
    /// Number of audit rows exported.
    pub rows: usize,
}

/// Export the `audit_log` table (optionally windowed by unix-millis
/// timestamps) to `~/.cortex/audit-export-<ts>.<jsonl|csv>`, redacted.
/// Mirrors `export_diagnostics`: the file lands on disk and the UI shows
/// its path.
#[tauri::command]
pub async fn export_audit_log(
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    format: String,
    store: State<'_, TracingStore>,
) -> Result<AuditLogExport, String> {
    let rows = store.audit_between(from_ts, to_ts).map_err(|e| e.to_string())?;
    let content = format_audit_rows(&rows, &format)?;
    let ext = format.trim().to_ascii_lowercase();
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    let cortex_dir = home.join(".cortex");
    std::fs::create_dir_all(&cortex_dir).map_err(|e| e.to_string())?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let out_path = cortex_dir.join(format!("audit-export-{ts}.{ext}"));
    std::fs::write(&out_path, content).map_err(|e| e.to_string())?;
    Ok(AuditLogExport {
        path: out_path.to_string_lossy().into_owned(),
        rows: rows.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- redactor: keys & tokens (must NEVER leak) ----

    #[test]
    fn redacts_provider_api_keys() {
        for sample in [
            "ANTHROPIC_API_KEY=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx",
            "openai sk-proj-1234567890abcdefghijklmn",
            "bare sk-abcdefghijklmnopqrstuvwxyz012345",
        ] {
            let out = redact_diagnostics(sample);
            assert!(!out.contains("sk-ant"), "leaked anthropic key: {out}");
            assert!(!out.contains("sk-proj"), "leaked openai key: {out}");
            assert!(out.contains("[REDACTED]"), "no mask in: {out}");
        }
    }

    #[test]
    fn redacts_forge_and_slack_tokens() {
        let gh = redact_diagnostics("ghp_0123456789abcdefghijABCDEFG");
        assert!(!gh.contains("ghp_0123"), "leaked: {gh}");
        let pat = redact_diagnostics("github_pat_11ABCDE_abcdefghijklmnop");
        assert!(!pat.contains("github_pat_11"), "leaked: {pat}");
        let slack = redact_diagnostics("xoxb-12345678901-abcdefghij");
        assert!(!slack.contains("xoxb-12345678901"), "leaked: {slack}");
    }

    #[test]
    fn redacts_bearer_jwt_aws_and_hex_keys() {
        let bearer = redact_diagnostics("Authorization: Bearer abc.def-123_xyz");
        assert!(!bearer.contains("abc.def-123_xyz"), "leaked: {bearer}");
        let jwt = redact_diagnostics("eyJhbGc.eyJzdWIiOiIx.SflKxwRJSMeKKF2QT4");
        assert!(!jwt.contains("eyJhbGc."), "leaked: {jwt}");
        let aws = redact_diagnostics("AKIAIOSFODNN7EXAMPLE");
        assert!(!aws.contains("AKIAIOSFODNN7"), "leaked: {aws}");
        // The gateway backend key shape: 64-char hex.
        let hex = "a".repeat(64);
        let out = redact_diagnostics(&format!("key {hex} end"));
        assert!(!out.contains(&hex), "leaked 64-hex key: {out}");
    }

    #[test]
    fn redacts_key_value_assignments_in_json_config() {
        let cfg = r#"{"gateway_api_key": "supersecretvalue123", "password": "hunter2pass"}"#;
        let out = redact_diagnostics(cfg);
        assert!(!out.contains("supersecretvalue123"), "leaked: {out}");
        assert!(!out.contains("hunter2pass"), "leaked: {out}");
        // Key names stay readable for debugging.
        assert!(out.contains("gateway_api_key"));
    }

    #[test]
    fn redacts_pem_private_key_blocks() {
        let pem = "x\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3NzaC1\n-----END OPENSSH PRIVATE KEY-----\ny";
        let out = redact_diagnostics(pem);
        assert!(!out.contains("AAAAB3NzaC1"), "leaked: {out}");
    }

    // ---- redactor: private / tailnet IPs ----

    #[test]
    fn redacts_rfc1918_ranges_with_ports() {
        for sample in [
            "http://192.168.1.10:8642",
            "ssh root@10.0.0.5",
            "172.16.4.20:443 latency",
            "172.31.255.255",
        ] {
            let out = redact_diagnostics(sample);
            assert!(out.contains(IP_MASK), "no mask in: {out}");
            assert!(!out.contains("192.168."), "leaked: {out}");
            assert!(!out.contains("10.0.0.5"), "leaked: {out}");
            assert!(!out.contains("172.16."), "leaked: {out}");
            assert!(!out.contains("172.31."), "leaked: {out}");
        }
    }

    #[test]
    fn redacts_tailscale_cgnat_range() {
        let out = redact_diagnostics("peer at 100.64.0.1 and 100.127.255.254");
        assert!(!out.contains("100.64.0.1"), "leaked: {out}");
        assert!(!out.contains("100.127.255.254"), "leaked: {out}");
    }

    #[test]
    fn leaves_public_ips_alone() {
        for ip in ["8.8.8.8", "100.1.1.1", "100.128.0.1", "172.32.0.1", "192.169.0.1", "11.0.0.1"] {
            let out = redact_diagnostics(&format!("ping {ip} ok"));
            assert!(out.contains(ip), "over-redacted public ip {ip}: {out}");
        }
    }

    // ---- redactor: home paths ----

    #[test]
    fn collapses_home_paths_keeping_tail() {
        let out = redact_diagnostics("/home/exampleuser/projects/cortex/src/lib.rs:42");
        assert!(!out.contains("exampleuser"), "leaked username: {out}");
        assert!(out.contains("~/projects/cortex/src/lib.rs:42"), "tail lost: {out}");

        let mac = redact_diagnostics("/Users/exampleuser/Library/Logs/app.log");
        assert!(!mac.contains("exampleuser"), "leaked: {mac}");
        assert!(mac.contains("~/Library/Logs/app.log"), "tail lost: {mac}");

        let win = redact_diagnostics(r"C:\Users\exampleuser\AppData\cortex.log");
        assert!(!win.contains("exampleuser"), "leaked: {win}");
    }

    #[test]
    fn leaves_prose_and_relative_paths_untouched() {
        let prose = "chat_send failed in src/commands/chat.rs:120 after 3 retries";
        assert_eq!(redact_diagnostics(prose), prose);
    }

    // ---- bundle builder: end-to-end no-leak property ----

    fn poisoned_config() -> ConfigSnapshot {
        ConfigSnapshot {
            gateway_base_url: "http://192.168.1.10:8642".into(),
            gateway_model: "gateway-agent".into(),
            default_project_root: Some("/home/exampleuser/projects".into()),
            ollama_base_url: "http://192.168.1.30:11434".into(),
            ollama_model: "qwen2.5:14b".into(),
            obsidian_vault: Some("/home/exampleuser/vault".into()),
            sandbox_tier: None,
            reasoning_effort: None,
            active_profile: None,
            git_server_url: Some("http://192.168.1.20:3000".into()),
            git_server_cloned_path: None,
            runtime_mode: "homelab".into(),
        }
    }

    #[test]
    fn bundle_never_contains_secrets_ips_or_usernames() {
        let crashes = r#"[{"message": "panic: bad key sk-ant-api03-LeakedKeyAbCdEfGh99 at http://192.168.1.10:8642", "stack": "/home/exampleuser/x.rs:1"}]"#;
        let sessions = r#"[{"session_id": "s1", "project_root": "/home/exampleuser/projects/cortex"}]"#;
        let cfg_files = vec![(
            "git-config.json".to_string(),
            r#"{"git_server_url": "http://192.168.1.20:3000", "token": "ghp_0123456789abcdefghijABCDEFG"}"#.to_string(),
        )];
        let files = build_bundle_files(&poisoned_config(), crashes, sessions, &cfg_files);

        assert!(files.iter().any(|(n, _)| n == "meta.json"));
        assert!(files.iter().any(|(n, _)| n == "config.json"));
        assert!(files.iter().any(|(n, _)| n == "crash-log.json"));
        assert!(files.iter().any(|(n, _)| n == "sessions.json"));
        assert!(files.iter().any(|(n, _)| n == "cortex-config/git-config.json"));

        for (name, content) in &files {
            assert!(!content.contains("sk-ant"), "{name} leaked a key: {content}");
            assert!(!content.contains("ghp_0123"), "{name} leaked a PAT: {content}");
            assert!(!content.contains("192.168."), "{name} leaked an IP: {content}");
            assert!(!content.contains("exampleuser"), "{name} leaked username: {content}");
        }
        // Useful debugging context survives redaction.
        let config = &files.iter().find(|(n, _)| n == "config.json").unwrap().1;
        assert!(config.contains("gateway-agent"));
        assert!(config.contains(IP_MASK));
    }

    #[test]
    fn meta_reports_version_and_variant() {
        let files = build_bundle_files(&poisoned_config(), "[]", "[]", &[]);
        let meta = &files.iter().find(|(n, _)| n == "meta.json").unwrap().1;
        assert!(meta.contains(env!("CARGO_PKG_VERSION")));
        assert!(meta.contains("homelab") || meta.contains("standalone"));
    }

    #[test]
    fn allowlist_excludes_key_vault() {
        assert!(
            !CORTEX_CONFIG_ALLOWLIST.iter().any(|n| n.contains("keys")),
            "the encrypted key vault must never be bundled"
        );
    }

    // ---- archive writing ----

    #[test]
    fn writes_a_readable_tar_gz() {
        let dir = std::env::temp_dir().join(format!("cortex-diag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diag.tar.gz");
        let files = vec![("meta.json".to_string(), "{\"ok\":true}".to_string())];
        write_tar_gz(&path, &files).unwrap();

        // Round-trip: gunzip + untar and confirm the entry + content.
        let f = std::fs::File::open(&path).unwrap();
        let gz = flate2::read::GzDecoder::new(f);
        let mut ar = tar::Archive::new(gz);
        let mut names = Vec::new();
        for entry in ar.entries().unwrap() {
            let mut e = entry.unwrap();
            names.push(e.path().unwrap().to_string_lossy().into_owned());
            let mut s = String::new();
            std::io::Read::read_to_string(&mut e, &mut s).unwrap();
            assert_eq!(s, "{\"ok\":true}");
        }
        assert_eq!(names, vec!["diagnostics/meta.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- audit export (Safe Mode / issue 004) ----

    fn poisoned_audit_rows() -> Vec<AuditRow> {
        vec![
            AuditRow {
                ts: 1,
                session_id: Some("s1".into()),
                agent_id: Some("claude-cli".into()),
                action: "safe-mode.toggled".into(),
                detail: Some(r#"{"enabled":true}"#.into()),
            },
            AuditRow {
                ts: 2,
                session_id: Some("s1".into()),
                agent_id: None,
                action: "safe-mode.tool-call".into(),
                detail: Some(
                    "shell_exec: curl -H \"Authorization: Bearer sk-abcdefghijklmnopqrstuvwxyz0123\" \
                     && export password=hunter2secret, token ghp_0123456789abcdefghijABCDEFG"
                        .into(),
                ),
            },
            AuditRow {
                ts: 3,
                session_id: None,
                agent_id: None,
                // A hostile value with CSV metacharacters must not break rows.
                action: "weird,\"action\"\nline".into(),
                detail: None,
            },
        ]
    }

    /// The redaction property: no secret shape survives into either export
    /// format, while non-secret context stays readable.
    #[test]
    fn audit_export_redacts_secrets_in_both_formats() {
        let rows = poisoned_audit_rows();
        for format in ["jsonl", "csv"] {
            let out = format_audit_rows(&rows, format).unwrap();
            assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz0123"), "{format} leaked key: {out}");
            assert!(!out.contains("hunter2secret"), "{format} leaked password: {out}");
            assert!(!out.contains("ghp_0123456789"), "{format} leaked PAT: {out}");
            assert!(out.contains("[REDACTED]"), "{format} has no mask: {out}");
            // Useful context survives.
            assert!(out.contains("safe-mode.toggled"), "{format} lost action: {out}");
            assert!(out.contains("shell_exec"), "{format} lost tool name: {out}");
        }
    }

    /// JSONL rows must each parse back as JSON (the `jq` acceptance check).
    #[test]
    fn audit_export_jsonl_lines_are_valid_json() {
        let out = format_audit_rows(&poisoned_audit_rows(), "jsonl").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            assert!(v.get("ts").is_some());
            assert!(v.get("action").is_some());
        }
    }

    /// CSV must keep one logical row per record even with embedded quotes,
    /// commas, and newlines in a field (RFC 4180 quoting).
    #[test]
    fn audit_export_csv_quotes_hostile_fields() {
        let out = format_audit_rows(&poisoned_audit_rows(), "csv").unwrap();
        assert!(out.starts_with("ts,session_id,agent_id,action,detail\n"));
        // The hostile action's quotes are doubled and the field is wrapped.
        assert!(out.contains("\"weird,\"\"action\"\"\nline\""), "bad quoting: {out}");
    }

    #[test]
    fn audit_export_rejects_unknown_format() {
        assert!(format_audit_rows(&[], "xml").is_err());
        assert!(format_audit_rows(&[], "").is_err());
        // Empty row set is fine — an empty (or header-only) file, not an error.
        assert_eq!(format_audit_rows(&[], "jsonl").unwrap(), "");
        assert_eq!(
            format_audit_rows(&[], "CSV").unwrap(),
            "ts,session_id,agent_id,action,detail\n"
        );
    }

    // ---- session metadata (no contents) ----

    #[test]
    fn session_meta_has_no_message_content() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../observability/schema.sql")).unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, ts, role, content, project_root)
             VALUES ('m1', 's1', 100, 'user', 'TOP SECRET PROMPT', '/home/u/proj')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, ts, role, content)
             VALUES ('m2', 's1', 200, 'assistant', 'SECRET REPLY')",
            [],
        )
        .unwrap();
        let conn = Mutex::new(conn);
        let meta = recent_session_meta(&conn, 10).unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].session_id, "s1");
        assert_eq!(meta[0].message_count, 2);
        assert_eq!(meta[0].first_ts, 100);
        assert_eq!(meta[0].last_ts, 200);
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("TOP SECRET"), "leaked content: {json}");
        assert!(!json.contains("SECRET REPLY"), "leaked content: {json}");
    }
}
