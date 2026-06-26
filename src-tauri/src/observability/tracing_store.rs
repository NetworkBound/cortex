use crate::agents::AgentEvent;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Clone)]
pub struct TracingStore {
    inner: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub id: String,
    pub parent_id: Option<String>,
    pub trace_id: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub name: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: String,
    pub attributes: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub trace_id: String,
    pub session_id: String,
    pub started_at: i64,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthRow {
    pub source: String,
    pub ts: i64,
    pub ok: bool,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueRow {
    pub fingerprint: String,
    pub agent_id: Option<String>,
    pub error_class: Option<String>,
    pub message: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub last_active_ms: i64,
    pub message_count: i64,
    pub agents: Vec<String>,
    pub first_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub ts: i64,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub action: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub ts: i64,
    pub name: String,
    pub span_name: String,
    pub agent_id: Option<String>,
    pub payload: serde_json::Value,
}

/// One row of the "recent chats" list: a distinct chat session drawn from the
/// `messages` table (Cortex's real chat history), with a derived title + preview
/// so the mobile app can show a resumable session list without loading every
/// message. `title` is the first user message truncated; `preview` is the most
/// recent message content truncated.
#[derive(Debug, Clone, Serialize)]
pub struct RecentChatSession {
    pub id: String,
    pub title: String,
    pub last_ts: i64,
    pub message_count: i64,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub ts: i64,
    pub role: String,
    pub snippet: String,
}

/// Persisted chat message. The messages table is not created by the
/// current schema; the methods below treat its absence as "no history"
/// rather than an error, so callers can fall back to in-memory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub session_id: String,
    pub ts: i64,
    pub role: String,
    pub agent_id: Option<String>,
    pub content: String,
    pub run_id: Option<String>,
    pub reasoning: Option<String>,
    pub project_root: Option<String>,
}

impl TracingStore {
    pub fn open_default() -> anyhow::Result<Self> {
        let dir = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("no data_local_dir"))?
            .join("cortex");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("cortex-local.db");
        Self::open_at(path)
    }

    pub fn open_at(path: PathBuf) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn);
        Ok(Self { inner: Arc::new(Mutex::new(conn)) })
    }

    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-mem sqlite");
        conn.execute_batch(SCHEMA).expect("schema");
        Self::migrate(&conn);
        Self { inner: Arc::new(Mutex::new(conn)) }
    }

    /// Additive column migrations for databases created before the column
    /// existed in `schema.sql` (`CREATE TABLE IF NOT EXISTS` never alters an
    /// existing table). Each statement is a no-op failure ("duplicate column
    /// name") on up-to-date databases, so this is safe to run on every open.
    fn migrate(conn: &Connection) {
        let _ = conn.execute("ALTER TABLE lane_runs ADD COLUMN merged_at INTEGER", []);
    }

    pub fn shared_connection(&self) -> Arc<Mutex<Connection>> {
        self.inner.clone()
    }

    pub fn events_for_trace(&self, trace_id: &str) -> anyhow::Result<Vec<TraceEvent>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT e.ts, e.name, s.name, s.agent_id, e.payload
             FROM events e JOIN spans s ON s.id = e.span_id
             WHERE s.trace_id = ?1
             ORDER BY e.ts ASC
             LIMIT 500",
        )?;
        let rows = stmt.query_map([trace_id], |r| {
            let payload_str: String = r.get(4)?;
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);
            Ok(TraceEvent {
                ts: r.get(0)?,
                name: r.get(1)?,
                span_name: r.get(2)?,
                agent_id: r.get(3)?,
                payload,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn tokens_by_session(&self, limit: usize) -> anyhow::Result<Vec<crate::usage::SessionTokens>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT s.session_id,
                    MAX(s.started_at) as last_active,
                    COALESCE(SUM(CAST(json_extract(e.payload, '$.tokens') AS INTEGER)), 0) as total,
                    COUNT(DISTINCT s.id) as runs
             FROM spans s LEFT JOIN events e ON e.span_id = s.id AND e.name = 'done'
             WHERE s.name = 'agent.run'
             GROUP BY s.session_id
             ORDER BY last_active DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(crate::usage::SessionTokens {
                session_id: r.get(0)?,
                last_active_ms: r.get(1)?,
                total_tokens: r.get::<_, i64>(2)?.max(0) as u64,
                runs: r.get::<_, i64>(3)?.max(0) as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn tokens_by_provider(&self, limit: usize) -> anyhow::Result<Vec<crate::usage::ProviderUsage>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT s.agent_id,
                    COALESCE(SUM(CAST(json_extract(e.payload, '$.tokens') AS INTEGER)), 0) as total,
                    COUNT(DISTINCT s.id) as runs
             FROM spans s LEFT JOIN events e ON e.span_id = s.id AND e.name = 'done'
             WHERE s.name = 'agent.run' AND s.agent_id IS NOT NULL
             GROUP BY s.agent_id
             ORDER BY total DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(crate::usage::ProviderUsage {
                agent_id: r.get(0)?,
                total_tokens: r.get::<_, i64>(1)?.max(0) as u64,
                runs: r.get::<_, i64>(2)?.max(0) as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// Token/run totals grouped by the *effective model* recorded on each
    /// `agent.run` span (in its `attributes.model`). Distinct from
    /// `tokens_by_provider`, which groups by the adapter (`agent_id`): one
    /// adapter (e.g. `gateway-remote`) routes to many upstream models, so this
    /// is the breakdown that attributes climbing token spend to the model that
    /// actually produced it. Runs that never recorded a model are omitted.
    pub fn tokens_by_model(&self, limit: usize) -> anyhow::Result<Vec<crate::usage::ModelUsage>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT json_extract(s.attributes, '$.model') as model,
                    MAX(s.agent_id) as agent_id,
                    COALESCE(SUM(CAST(json_extract(e.payload, '$.tokens') AS INTEGER)), 0) as total,
                    COUNT(DISTINCT s.id) as runs
             FROM spans s LEFT JOIN events e ON e.span_id = s.id AND e.name = 'done'
             WHERE s.name = 'agent.run' AND json_extract(s.attributes, '$.model') IS NOT NULL
             GROUP BY model
             ORDER BY total DESC, runs DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(crate::usage::ModelUsage {
                model: r.get(0)?,
                agent_id: r.get(1)?,
                total_tokens: r.get::<_, i64>(2)?.max(0) as u64,
                runs: r.get::<_, i64>(3)?.max(0) as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn record_chat_turn(
        &self,
        trace_id: &str,
        session_id: &str,
        message: &str,
        picked_agents: &[String],
    ) -> anyhow::Result<()> {
        let span_id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let preview: String = message.chars().take(120).collect();
        let attrs = serde_json::json!({
            "message_chars": message.chars().count(),
            "picked_agents": picked_agents,
            "first_message_preview": preview,
        });
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO spans (id, parent_id, trace_id, session_id, agent_id, name, started_at, ended_at, status, attributes)
             VALUES (?1, NULL, ?2, ?3, NULL, 'chat.turn', ?4, ?4, 'ok', ?5)",
            params![span_id, trace_id, session_id, now, attrs.to_string()],
        )?;
        Ok(())
    }

    pub fn start_agent_run(
        &self,
        span_id: &str,
        trace_id: &str,
        session_id: &str,
        agent_id: &str,
        model: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        // Persist the *effective model* (per-request routing pick) on the span
        // so `tokens_by_model` can attribute spend to the model that produced
        // it. Empty/None falls back to an empty attribute object — the model
        // breakdown simply skips runs without one.
        let attrs = match model {
            Some(m) if !m.is_empty() => serde_json::json!({ "model": m }).to_string(),
            _ => "{}".to_string(),
        };
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO spans (id, parent_id, trace_id, session_id, agent_id, name, started_at, ended_at, status, attributes)
             VALUES (?1, NULL, ?2, ?3, ?4, 'agent.run', ?5, NULL, 'running', ?6)",
            params![span_id, trace_id, session_id, agent_id, now, attrs],
        )?;
        Ok(())
    }

    pub fn record_event(&self, span_id: &str, event: &AgentEvent) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let (name, payload) = event_to_record(event);
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO events (span_id, ts, name, payload) VALUES (?1, ?2, ?3, ?4)",
            params![span_id, now, name, payload.to_string()],
        )?;
        // Issue tracking: dedupe error events into issues
        if let AgentEvent::Error { message } = event {
            let fp = simple_fingerprint(message);
            // Attribute the issue to the agent that owns the span, when known,
            // so the UI can group/filter issues by agent.
            let agent_id: Option<String> = conn
                .query_row(
                    "SELECT agent_id FROM spans WHERE id = ?1",
                    params![span_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            conn.execute(
                "INSERT INTO issues (fingerprint, agent_id, error_class, message, first_seen, last_seen, count, example_span, status)
                 VALUES (?1, ?6, ?2, ?3, ?4, ?4, 1, ?5, 'unresolved')
                 ON CONFLICT(fingerprint) DO UPDATE SET last_seen = ?4, count = count + 1",
                params![fp, error_class(message), message, now, span_id, agent_id],
            )?;
        }
        Ok(())
    }

    pub fn finish_agent_run(&self, span_id: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.inner.lock();
        conn.execute(
            "UPDATE spans SET ended_at = ?1, status = CASE status WHEN 'running' THEN 'ok' ELSE status END WHERE id = ?2",
            params![now, span_id],
        )?;
        Ok(())
    }

    pub fn recent_traces(&self, limit: usize) -> anyhow::Result<Vec<Trace>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT trace_id, session_id, MIN(started_at) FROM spans GROUP BY trace_id ORDER BY MIN(started_at) DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;

        let mut traces = Vec::new();
        for r in rows.flatten() {
            let (trace_id, session_id, started_at) = r;
            let mut s_stmt = conn.prepare(
                "SELECT id, parent_id, trace_id, session_id, agent_id, name, started_at, ended_at, status, attributes
                 FROM spans WHERE trace_id = ?1 ORDER BY started_at ASC",
            )?;
            let spans = s_stmt
                .query_map(params![trace_id], |r| {
                    Ok(Span {
                        id: r.get(0)?,
                        parent_id: r.get(1)?,
                        trace_id: r.get(2)?,
                        session_id: r.get(3)?,
                        agent_id: r.get(4)?,
                        name: r.get(5)?,
                        started_at: r.get(6)?,
                        ended_at: r.get(7)?,
                        status: r.get(8)?,
                        attributes: r
                            .get::<_, String>(9)
                            .ok()
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or(serde_json::json!({})),
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            traces.push(Trace { trace_id, session_id, started_at, spans });
        }
        Ok(traces)
    }

    pub fn record_health(&self, source: &str, ok: bool, latency_ms: Option<i64>, payload: Option<&str>) -> anyhow::Result<()> {
        let conn = self.inner.lock();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO health_samples (source, ts, ok, latency_ms, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source, now, ok as i64, latency_ms, payload],
        )?;
        Ok(())
    }

    pub fn latest_health(&self) -> anyhow::Result<Vec<HealthRow>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT source, ts, ok, latency_ms FROM health_samples
             WHERE (source, ts) IN (SELECT source, MAX(ts) FROM health_samples GROUP BY source)
             ORDER BY source ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(HealthRow {
                source: r.get(0)?,
                ts: r.get(1)?,
                ok: r.get::<_, i64>(2)? != 0,
                latency_ms: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn recent_issues(&self, limit: usize) -> anyhow::Result<Vec<IssueRow>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, agent_id, error_class, message, first_seen, last_seen, count
             FROM issues
             WHERE status != 'resolved'
             ORDER BY last_seen DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(IssueRow {
                fingerprint: r.get(0)?,
                agent_id: r.get(1)?,
                error_class: r.get(2)?,
                message: r.get(3)?,
                first_seen: r.get(4)?,
                last_seen: r.get(5)?,
                count: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn record_audit(&self, session_id: Option<&str>, agent_id: Option<&str>, action: &str, detail: Option<&str>) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.inner.lock();
        conn.execute(
            "INSERT INTO audit_log (ts, session_id, agent_id, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now, session_id, agent_id, action, detail],
        )?;
        Ok(())
    }

    pub fn recent_sessions(&self, limit: usize) -> anyhow::Result<Vec<SessionSummary>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id, MAX(started_at) AS last_active, COUNT(DISTINCT trace_id) AS msgs,
                    GROUP_CONCAT(DISTINCT agent_id) AS agents
             FROM spans
             WHERE session_id IS NOT NULL
             GROUP BY session_id
             ORDER BY last_active DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let agents_str: Option<String> = r.get(3).ok();
            let agents: Vec<String> = agents_str
                .as_deref()
                .map(|s| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect())
                .unwrap_or_default();
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, agents))
        })?;
        let mut out: Vec<SessionSummary> = Vec::new();
        for r in rows.flatten() {
            let (session_id, last_active_ms, message_count, agents) = r;
            // Try to grab the first user message from chat.turn attributes
            let first_message: Option<String> = conn
                .query_row(
                    "SELECT attributes FROM spans
                     WHERE session_id = ?1 AND name = 'chat.turn'
                     ORDER BY started_at ASC LIMIT 1",
                    params![session_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|attrs| serde_json::from_str::<serde_json::Value>(&attrs).ok())
                .and_then(|v| {
                    v.get("first_message_preview")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                });
            out.push(SessionSummary {
                session_id,
                last_active_ms,
                message_count,
                agents,
                first_message,
            });
        }
        Ok(out)
    }

    pub fn recent_audit(&self, limit: usize) -> anyhow::Result<Vec<AuditRow>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT ts, session_id, agent_id, action, detail
             FROM audit_log ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(AuditRow {
                ts: r.get(0)?,
                session_id: r.get(1)?,
                agent_id: r.get(2)?,
                action: r.get(3)?,
                detail: r.get(4)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn search_messages(&self, query: &str, limit: i64) -> anyhow::Result<Vec<SessionSearchHit>> {
        // Escape LIKE wildcards (`%`, `_`) and the escape char itself so the
        // user's query is matched literally rather than as a pattern.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like = format!("%{}%", escaped);
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id, ts, role, substr(content, 1, 200)
             FROM messages WHERE content LIKE ?1 ESCAPE '\\' ORDER BY ts DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit], |r| {
            Ok(SessionSearchHit {
                session_id: r.get(0)?,
                ts: r.get(1)?,
                role: r.get(2)?,
                snippet: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Return `(total_chars, message_count)` for a session, summing
    /// `LENGTH(content)` (byte count — SQLite has no chars function).
    /// Returns `(0, 0)` if the `messages` table doesn't exist yet, which
    /// is the expected case on builds without session persistence wired.
    pub fn sum_session_chars(&self, session_id: &str) -> anyhow::Result<(usize, usize)> {
        let conn = self.inner.lock();
        let result: rusqlite::Result<(i64, i64)> = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(content)), 0), COUNT(*) FROM messages WHERE session_id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match result {
            Ok((chars, count)) => Ok((chars.max(0) as usize, count.max(0) as usize)),
            Err(_) => Ok((0, 0)),
        }
    }

    /// Count distinct sessions whose messages carry the given `agent_id` tag.
    /// Used by `history_sync` to report how many conversations were imported
    /// from a given provider (the import pipeline tags every message with
    /// `agent_id = "import:<source>"`). Graceful: returns 0 on any error.
    pub fn count_imported_sessions(&self, agent_id: &str) -> anyhow::Result<i64> {
        let conn = self.inner.lock();
        let result: rusqlite::Result<i64> = conn.query_row(
            "SELECT COUNT(DISTINCT session_id) FROM messages WHERE agent_id = ?1",
            params![agent_id],
            |r| r.get(0),
        );
        Ok(result.unwrap_or(0).max(0))
    }

    /// Latest assistant-role message content for a session, or `None`.
    /// Same graceful-fallback behaviour as `sum_session_chars`.
    pub fn latest_assistant_content(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.inner.lock();
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT content FROM messages
             WHERE session_id = ?1 AND role = 'assistant'
             ORDER BY ts DESC LIMIT 1",
            params![session_id],
            |r| r.get(0),
        );
        match result {
            Ok(content) => Ok(Some(content)),
            Err(_) => Ok(None),
        }
    }

    /// List distinct recent chat sessions from the `messages` table, newest
    /// first. Each row carries a derived `title` (first non-system message,
    /// truncated), `preview` (most recent message, truncated), `last_ts`, and
    /// `message_count`. Sessions whose only rows are auto-injected `system`
    /// context are still listed but fall back to the latest content for a title.
    ///
    /// Index-friendly: the grouping + max(ts) ride the `idx_messages_session`
    /// index `(session_id, ts)`. Returns an empty vec when the `messages` table
    /// is absent (same graceful-fallback contract as the other readers).
    pub fn recent_chat_sessions(&self, limit: usize) -> anyhow::Result<Vec<RecentChatSession>> {
        let conn = self.inner.lock();
        // Per-session aggregate: last activity + count. Ordered newest-first.
        let stmt = conn.prepare(
            "SELECT session_id, MAX(ts) AS last_ts, COUNT(*) AS n
             FROM messages
             GROUP BY session_id
             ORDER BY last_ts DESC
             LIMIT ?1",
        );
        let mut stmt = match stmt {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let heads = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        let mut out = Vec::with_capacity(heads.len());
        for (session_id, last_ts, message_count) in heads {
            // Title: first non-system message (the user's opening line). Falls
            // back to the very first message of any role if the session is all
            // system context.
            let title_raw: Option<String> = conn
                .query_row(
                    "SELECT content FROM messages
                     WHERE session_id = ?1 AND role != 'system'
                     ORDER BY ts ASC LIMIT 1",
                    params![session_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .or_else(|| {
                    conn.query_row(
                        "SELECT content FROM messages WHERE session_id = ?1 ORDER BY ts ASC LIMIT 1",
                        params![session_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                });
            // Preview: most-recent message content.
            let preview_raw: Option<String> = conn
                .query_row(
                    "SELECT content FROM messages WHERE session_id = ?1 ORDER BY ts DESC LIMIT 1",
                    params![session_id],
                    |r| r.get::<_, String>(0),
                )
                .ok();

            let title = title_raw
                .as_deref()
                .map(|s| truncate_title(s, 80))
                .unwrap_or_else(|| "New chat".to_string());
            let preview = preview_raw
                .as_deref()
                .map(|s| truncate_title(s, 140))
                .unwrap_or_default();

            out.push(RecentChatSession {
                id: session_id,
                title,
                last_ts,
                message_count,
                preview,
            });
        }
        Ok(out)
    }

    /// Load every message for a session, ordered chronologically. Returns
    /// an empty vec when the messages table is missing or the session has
    /// no rows — never errors on schema mismatch.
    pub fn load_session_messages(&self, session_id: &str) -> anyhow::Result<Vec<StoredMessage>> {
        let conn = self.inner.lock();
        let stmt = conn.prepare(
            "SELECT id, session_id, ts, role, agent_id, content, run_id, reasoning, project_root
             FROM messages WHERE session_id = ?1 ORDER BY ts ASC",
        );
        let mut stmt = match stmt {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(StoredMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                ts: r.get(2)?,
                role: r.get(3)?,
                agent_id: r.get(4)?,
                content: r.get(5)?,
                run_id: r.get(6)?,
                reasoning: r.get(7)?,
                project_root: r.get(8)?,
            })
        });
        match rows {
            Ok(iter) => Ok(iter.filter_map(|r| r.ok()).collect()),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Persist a chat message. Best-effort: silently no-ops if the messages
    /// table isn't present (the schema currently doesn't create it).
    pub fn record_message(&self, msg: &StoredMessage) -> anyhow::Result<()> {
        let conn = self.inner.lock();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO messages
             (id, session_id, ts, role, agent_id, content, run_id, reasoning, project_root)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                msg.id,
                msg.session_id,
                msg.ts,
                msg.role,
                msg.agent_id,
                msg.content,
                msg.run_id,
                msg.reasoning,
                msg.project_root,
            ],
        );
        Ok(())
    }

    /// Delete every stored message for `session_id`. Used by the e2e probe to
    /// clean up throwaway fixture sessions; nothing user-facing calls this.
    pub fn delete_session_messages(&self, session_id: &str) -> anyhow::Result<usize> {
        let conn = self.inner.lock();
        let n = conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(n)
    }

    /// Most-recently active session_id for the given project root, or None.
    /// Returns None when no messages table exists or no matching session.
    pub fn latest_session_for_project(&self, project_root: &str) -> anyhow::Result<Option<String>> {
        let conn = self.inner.lock();
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT session_id FROM messages
             WHERE project_root = ?1
             ORDER BY ts DESC LIMIT 1",
            params![project_root],
            |r| r.get(0),
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(_) => Ok(None),
        }
    }
}

fn event_to_record(event: &AgentEvent) -> (&'static str, serde_json::Value) {
    match event {
        AgentEvent::Started { agent_id, run_id } => ("started", serde_json::json!({ "agent_id": agent_id, "run_id": run_id })),
        AgentEvent::Token { delta } => ("token", serde_json::json!({ "chars": delta.chars().count() })),
        AgentEvent::Reasoning { text } => ("reasoning", serde_json::json!({ "chars": text.chars().count() })),
        AgentEvent::ToolCall { name, preview, .. } => ("tool_call", serde_json::json!({ "name": name, "preview": preview })),
        AgentEvent::ToolResult { name, ok, duration_ms, .. } => ("tool_result", serde_json::json!({ "name": name, "ok": ok, "duration_ms": duration_ms })),
        AgentEvent::FileEdit { path, lines_changed } => ("file_edit", serde_json::json!({ "path": path, "lines": lines_changed })),
        AgentEvent::ApprovalRequest { tool, .. } => ("approval_request", serde_json::json!({ "tool": tool })),
        AgentEvent::ApprovalResolved { choice, .. } => ("approval_resolved", serde_json::json!({ "choice": choice })),
        AgentEvent::Error { message } => ("error", serde_json::json!({ "message": message })),
        AgentEvent::Done { total_tokens, run_id } => ("done", serde_json::json!({ "tokens": total_tokens, "run_id": run_id })),
    }
}

/// Normalize a message for fingerprinting: collapse each run of ASCII digits
/// into a single `#` placeholder rather than deleting digits outright. This
/// keeps variable numeric tokens (timestamps, PIDs, offsets) from fragmenting
/// the same error while still preserving distinguishing numbers like HTTP
/// status codes (e.g. "401" vs "500" remain distinct as "#" in different
/// surrounding text), and avoids collapsing genuinely different errors that
/// differ only by digits into one issue.
fn normalize_for_fingerprint(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut prev_digit = false;
    for c in msg.chars() {
        if c.is_ascii_digit() {
            if !prev_digit {
                out.push('#');
            }
            prev_digit = true;
        } else {
            out.push(c);
            prev_digit = false;
        }
    }
    out
}

/// Collapse a message into a single-line title/preview of at most `max` chars.
/// Whitespace runs (including newlines) become a single space, leading/trailing
/// whitespace is trimmed, and an ellipsis marks truncation. Char-based so it
/// never splits a multi-byte grapheme.
fn truncate_title(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let head: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn simple_fingerprint(msg: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Normalize digit runs first, then truncate on a char boundary so the
    // distinguishing prefix is preserved without panicking on multi-byte text.
    let normalized = normalize_for_fingerprint(msg);
    let stripped = truncate_on_char_boundary(&normalized, 120);
    let mut h = DefaultHasher::new();
    stripped.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn error_class(msg: &str) -> String {
    if msg.contains("timeout") { "TimeoutError".into() }
    else if msg.contains("connection") || msg.contains("Connection") { "ConnectionError".into() }
    else if msg.contains("401") || msg.contains("unauthor") || msg.contains("Unauthor") { "AuthError".into() }
    else if msg.contains("429") || msg.contains("rate") { "RateLimitError".into() }
    else if msg.contains("500") || msg.contains("503") { "UpstreamError".into() }
    else { "AgentError".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the real recording + aggregation path against the production
    /// SQLite engine (in-memory): start agent runs tagged with an effective
    /// model, stream `done` events carrying token totals, and assert that
    /// usage actually *climbs* — both per-model (`tokens_by_model`) and the
    /// previously-broken per-provider/per-session totals, which extracted a
    /// non-existent `$.tokens.total` path and silently summed to zero.
    #[test]
    fn usage_attributes_tokens_by_model_and_climbs() {
        let store = TracingStore::in_memory();

        // run a model on the same adapter twice, plus a second model once.
        let runs = [
            ("gateway-remote", "claude-sonnet-4-6", 100u64),
            ("gateway-remote", "claude-sonnet-4-6", 50u64),
            ("gateway-remote", "gpt-5.5", 30u64),
        ];
        for (i, (agent, model, tokens)) in runs.iter().enumerate() {
            let span_id = format!("span-{i}");
            store
                .start_agent_run(&span_id, "trace-1", "sess-1", agent, Some(model))
                .unwrap();
            store
                .record_event(
                    &span_id,
                    &AgentEvent::Done { total_tokens: Some(*tokens), run_id: None },
                )
                .unwrap();
            store.finish_agent_run(&span_id).unwrap();
        }

        // by-model: two distinct models, sonnet's tokens summed (climbing).
        let by_model = store.tokens_by_model(10).unwrap();
        assert_eq!(by_model.len(), 2, "two distinct models recorded");
        let sonnet = by_model
            .iter()
            .find(|m| m.model == "claude-sonnet-4-6")
            .expect("sonnet present");
        assert_eq!(sonnet.total_tokens, 150, "sonnet tokens sum across runs");
        assert_eq!(sonnet.runs, 2);
        assert_eq!(sonnet.agent_id.as_deref(), Some("gateway-remote"));
        let gpt = by_model.iter().find(|m| m.model == "gpt-5.5").unwrap();
        assert_eq!(gpt.total_tokens, 30);
        assert_eq!(gpt.runs, 1);
        // ordered by total desc — sonnet (150) before gpt (30).
        assert_eq!(by_model[0].model, "claude-sonnet-4-6");

        // by-provider: the `$.tokens` extraction fix means the adapter total
        // now sums to the real 180 instead of the old silent 0.
        let by_provider = store.tokens_by_provider(10).unwrap();
        let gateway = by_provider
            .iter()
            .find(|p| p.agent_id == "gateway-remote")
            .unwrap();
        assert_eq!(gateway.total_tokens, 180, "provider total climbs (was 0)");
        assert_eq!(gateway.runs, 3);

        // by-session totals climb too (same fix).
        let by_session = store.tokens_by_session(10).unwrap();
        let sess = by_session.iter().find(|s| s.session_id == "sess-1").unwrap();
        assert_eq!(sess.total_tokens, 180);
        assert_eq!(sess.runs, 3);
    }

    /// Runs that never recorded a model are omitted from the by-model
    /// breakdown but still counted in the by-provider/session rollups.
    #[test]
    fn runs_without_model_are_excluded_from_by_model() {
        let store = TracingStore::in_memory();
        store
            .start_agent_run("s1", "t", "sess", "ollama", None)
            .unwrap();
        store
            .record_event("s1", &AgentEvent::Done { total_tokens: Some(42), run_id: None })
            .unwrap();
        assert!(store.tokens_by_model(10).unwrap().is_empty());
        assert_eq!(
            store.tokens_by_provider(10).unwrap()[0].total_tokens,
            42
        );
    }
}
