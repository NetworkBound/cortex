//! Local-only crash collection. Stores Rust panics and JS errors into the
//! `crash_log` table so we can surface them in the Observability panel.
//!
//! Never leaves the device — there is no network upload. The Sentry SDK
//! integration (in `sentry.rs`) is intentionally separate and opt-in.

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashRow {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub message: String,
    pub stack: Option<String>,
    pub build_hash: Option<String>,
}

/// Insert a single crash row. Errors are propagated so callers can decide
/// whether to log or swallow them — the panic hook explicitly swallows.
pub fn record_crash(
    conn: &Mutex<Connection>,
    kind: &str,
    message: &str,
    stack: Option<&str>,
    build_hash: Option<&str>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    let guard = conn.lock();
    guard.execute(
        "INSERT INTO crash_log (ts, kind, message, stack, build_hash, handled)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![now, kind, message, stack, build_hash],
    )?;
    Ok(())
}

/// Most-recent crashes first.
pub fn recent_crashes(conn: &Mutex<Connection>, limit: usize) -> anyhow::Result<Vec<CrashRow>> {
    let guard = conn.lock();
    let mut stmt = guard.prepare(
        "SELECT id, ts, kind, message, stack, build_hash
         FROM crash_log ORDER BY ts DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| {
        Ok(CrashRow {
            id: r.get(0)?,
            ts: r.get(1)?,
            kind: r.get(2)?,
            message: r.get(3)?,
            stack: r.get(4)?,
            build_hash: r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Install a global panic hook that writes panics into the crash_log.
/// Non-blocking: if the connection lock is contended at panic time we
/// bail silently rather than risk a deadlock during unwinding.
///
/// Chains the previous hook so we still get the default stderr trace.
pub fn install_panic_hook(conn: Arc<Mutex<Connection>>, build_hash: String) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let stack = location.as_deref();

        // try_lock is non-blocking — never wait on a poisoned/held lock
        // while the process is unwinding.
        if let Some(guard) = conn.try_lock() {
            let now = chrono::Utc::now().timestamp_millis();
            let _ = guard.execute(
                "INSERT INTO crash_log (ts, kind, message, stack, build_hash, handled)
                 VALUES (?1, 'rust_panic', ?2, ?3, ?4, 0)",
                params![now, message, stack, build_hash],
            );
        }

        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn record_and_query() {
        let c = fresh_conn();
        record_crash(&c, "js_error", "boom", Some("at foo"), Some("0.1.0")).unwrap();
        record_crash(&c, "rust_panic", "kaboom", None, Some("0.1.0")).unwrap();
        let rows = recent_crashes(&c, 10).unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0].kind, "rust_panic");
        assert_eq!(rows[1].stack.as_deref(), Some("at foo"));
    }

    #[test]
    fn limit_is_respected() {
        let c = fresh_conn();
        for i in 0..5 {
            record_crash(&c, "js_error", &format!("e{i}"), None, None).unwrap();
        }
        let rows = recent_crashes(&c, 3).unwrap();
        assert_eq!(rows.len(), 3);
    }
}
