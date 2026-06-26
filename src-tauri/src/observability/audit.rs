//! Append-only audit log for every agent-initiated tool call, file edit,
//! and shell exec. Lives at `$XDG_DATA_HOME/cortex/audit-YYYYMMDD.log` (jsonl).
//!
//! Per ADR-006 / Phase 7: never contains secrets or full file bodies — just
//! paths, command names, and timestamps. Rotated daily, 90-day retention.

use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry<'a> {
    pub ts: i64,
    pub session_id: &'a str,
    pub agent_id: &'a str,
    pub action: &'a str,
    pub detail: serde_json::Value,
}

/// Directory that holds the audit logs: `$XDG_DATA_HOME/cortex`. Returns an
/// error rather than falling back to an empty/relative path, so the
/// security-critical log never lands in a CWD-relative location.
fn audit_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("no data or home directory for audit log"))?;
    Ok(base.join("cortex"))
}

pub fn log_path() -> anyhow::Result<PathBuf> {
    let dir = audit_dir()?;
    // Propagate (don't swallow) the create failure so append() surfaces it
    // instead of silently writing the audit log somewhere unexpected.
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("audit-{}.log", chrono::Utc::now().format("%Y%m%d"))))
}

pub fn append(entry: AuditEntry) -> anyhow::Result<()> {
    let line = serde_json::to_string(&entry)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path()?)?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn prune_old(retention_days: i64) -> anyhow::Result<()> {
    let dir = audit_dir()?;
    if !dir.exists() { return Ok(()); }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days);
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("audit-") || !name.ends_with(".log") { continue; }
        let date_str = &name["audit-".len()..name.len() - 4];
        let parsed = chrono::NaiveDate::parse_from_str(date_str, "%Y%m%d");
        if let Ok(d) = parsed {
            if d < cutoff.date_naive() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}
