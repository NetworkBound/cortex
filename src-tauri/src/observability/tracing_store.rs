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

/// One row of the Agent Reliability Dashboard: aggregate outcomes for a single
/// grouping key (a provider/agent id, or a model). A LOCAL VIEW — `success` is
/// derived from span status + error events, not from any upstream ground truth,
/// and gateway-internal retries are invisible here (see the UI honesty label).
#[derive(Debug, Clone, Serialize)]
pub struct ReliabilityRow {
    /// Display key: the `agent_id` (provider rows) or `model` (model rows).
    pub key: String,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub runs: u64,
    pub ok_runs: u64,
    pub error_runs: u64,
    /// Finished-but-unknown / still-running spans (excluded from success_rate).
    pub running_runs: u64,
    /// `ok_runs / (ok_runs + error_runs)`, 0.0 when nothing has finished.
    pub success_rate: f64,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub avg_ms: Option<i64>,
    pub total_tokens: u64,
    /// Estimated spend (local heuristic: 50/50 split + prefix pricing table).
    pub est_usd: f64,
    pub top_error_class: Option<String>,
    pub last_run_ms: i64,
    /// Span id + session of the most recent failing run in this group — lets the
    /// dashboard deep-link a failing row straight into Run Replay.
    pub last_error_span: Option<String>,
    pub last_error_session: Option<String>,
}

/// Windowed reliability report: overall totals plus per-provider and per-model
/// breakdowns. Pure read-side over existing `spans`/`events`.
#[derive(Debug, Clone, Serialize)]
pub struct ReliabilityReport {
    pub since_ms: Option<i64>,
    pub generated_ms: i64,
    pub totals: ReliabilityRow,
    pub by_provider: Vec<ReliabilityRow>,
    pub by_model: Vec<ReliabilityRow>,
    /// Issue 008 full scope: aggregate MCP tool-call count/latency/cost drawn
    /// from the `tool_call`/`tool_result` events every model-initiated MCP
    /// call already emits (see `commands::chat::dispatch_mcp_chat_tool`).
    pub mcp_tools: McpToolsSummary,
}

/// Per-qualified-tool-name (`mcp__<server>__<tool>`) aggregate, one row per
/// distinct tool that was actually called in the window.
#[derive(Debug, Clone, Serialize)]
pub struct McpToolStat {
    pub name: String,
    pub calls: u64,
    pub ok_calls: u64,
    pub avg_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

/// Aggregate MCP tool-call activity across the window: overall count/latency
/// plus a per-tool breakdown. `est_usd` is a LOCAL, coarse estimate — it sums
/// the whole-run cost (same token→price accounting as [`ReliabilityRow`]) of
/// every `agent.run` that made at least one MCP tool call, not a per-call
/// figure (tool-call-level token attribution isn't tracked); a run with no
/// token/model data contributes 0.0, matching `build_reliability_row`.
#[derive(Debug, Clone, Serialize)]
pub struct McpToolsSummary {
    pub calls: u64,
    pub ok_calls: u64,
    pub avg_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub est_usd: f64,
    pub by_tool: Vec<McpToolStat>,
}

/// One row of the Run Replay run picker: a past `agent.run` reduced to a
/// selectable summary.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayRunSummary {
    pub span_id: String,
    pub session_id: String,
    pub trace_id: String,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub tokens: u64,
    pub had_error: bool,
    pub prompt_preview: Option<String>,
}

/// One ordered step in a run's replay timeline — the redacted event payload as
/// captured (`redacted_for_display` ran at write time).
#[derive(Debug, Clone, Serialize)]
pub struct ReplayStepRow {
    pub ts: i64,
    pub name: String,
    pub payload: serde_json::Value,
}

/// Full replay of one run: metadata + routing reason + ordered timeline.
#[derive(Debug, Clone, Serialize)]
pub struct RunReplay {
    pub span_id: String,
    pub session_id: String,
    pub trace_id: String,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub routing_reason: Option<String>,
    pub prompt_preview: Option<String>,
    pub total_tokens: u64,
    pub est_usd: f64,
    pub steps: Vec<ReplayStepRow>,
}

/// One `agent.run` span reduced to the fields the reliability aggregation needs.
struct RunRecord {
    span_id: String,
    session_id: String,
    agent_id: String,
    model: Option<String>,
    started: i64,
    ended: Option<i64>,
    status: String,
    tokens: u64,
    had_error: bool,
    err_msg: Option<String>,
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
        // Semantic-search index over chat messages (incl. imported Claude/ChatGPT
        // history). One row per (message, embedding-model); created lazily so it
        // needs no schema.sql change. `vec` is little-endian f32 bytes.
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS chat_embeddings (
                 message_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 ts         INTEGER NOT NULL,
                 role       TEXT NOT NULL,
                 text       TEXT NOT NULL,
                 model      TEXT NOT NULL,
                 dim        INTEGER NOT NULL,
                 vec        BLOB NOT NULL,
                 PRIMARY KEY (message_id, model)
             )",
            [],
        );
        // Per-project memory namespace (issue 010 full scope): the owning
        // project root for a chunk, or NULL for "global" content (Obsidian
        // vault, home-level instructions, chat messages with no project
        // context) that's always visible regardless of the active project.
        // NULL on every pre-existing row until the next reindex retags it —
        // that just means old rows keep today's behavior (always visible)
        // rather than being newly hidden by the isolation filter.
        let _ = conn.execute("ALTER TABLE chat_embeddings ADD COLUMN project_root TEXT", []);
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
        routing_reason: Option<&str>,
    ) -> anyhow::Result<()> {
        let span_id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let preview: String = message.chars().take(120).collect();
        // Persist the routing reason (redacted) so Run Replay can show *why*
        // this model/agent was chosen. Additive attribute; old rows lack it.
        let attrs = serde_json::json!({
            "message_chars": message.chars().count(),
            "picked_agents": picked_agents,
            "first_message_preview": preview,
            "routing_reason": routing_reason.map(crate::redact::redact_text),
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

    /// Audit rows in an optional `[from_ts, to_ts]` window (millis, inclusive,
    /// `None` = unbounded), oldest first — the natural order for an export
    /// file. Hard-capped so a runaway export can't balloon memory.
    pub fn audit_between(
        &self,
        from_ts: Option<i64>,
        to_ts: Option<i64>,
    ) -> anyhow::Result<Vec<AuditRow>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT ts, session_id, agent_id, action, detail
             FROM audit_log
             WHERE ts >= COALESCE(?1, ts) AND ts <= COALESCE(?2, ts)
             ORDER BY ts ASC LIMIT 100000",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts], |r| {
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

    // ── Semantic chat index (chat_embeddings) ──────────────────────────────

    /// Messages not yet embedded with `model`, oldest first. Returns
    /// `(message_id, session_id, ts, role, content, project_root)`. Drives
    /// incremental reindexing — only new/changed messages get embedded.
    /// `project_root` comes straight from the message's own column (set when
    /// the turn happened with a project active) so the embedding inherits the
    /// same per-project tag without a second lookup at retrieval time.
    pub fn messages_needing_embedding(
        &self,
        model: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<(String, String, i64, String, String, Option<String>)>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.ts, m.role, m.content, m.project_root
             FROM messages m
             LEFT JOIN chat_embeddings e
               ON e.message_id = m.id AND e.model = ?1
             WHERE e.message_id IS NULL AND TRIM(m.content) != ''
             ORDER BY m.ts ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![model, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Store (or replace) one message's embedding. `vec` is written as
    /// little-endian f32 bytes. `project_root` is the owning project's root
    /// path (chat: the message's own project; note: the project directory the
    /// source file lives under), or `None` for global/unscoped content that
    /// should remain visible regardless of which project is active.
    pub fn upsert_chat_embedding(
        &self,
        message_id: &str,
        session_id: &str,
        ts: i64,
        role: &str,
        text: &str,
        model: &str,
        vec: &[f32],
        project_root: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let conn = self.inner.lock();
        conn.execute(
            "INSERT OR REPLACE INTO chat_embeddings
                 (message_id, session_id, ts, role, text, model, dim, vec, project_root)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![message_id, session_id, ts, role, text, model, vec.len() as i64, bytes, project_root],
        )?;
        Ok(())
    }

    /// Load every stored embedding for `model`:
    /// `(message_id, session_id, ts, role, text, vector, project_root)`.
    pub fn all_chat_embeddings(
        &self,
        model: &str,
    ) -> anyhow::Result<Vec<(String, String, i64, String, String, Vec<f32>, Option<String>)>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT message_id, session_id, ts, role, text, vec, project_root
             FROM chat_embeddings WHERE model = ?1",
        )?;
        let rows = stmt.query_map(params![model], |r| {
            let blob: Vec<u8> = r.get(5)?;
            let vec: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                vec,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// `(path, ts)` for every note embedded under `model` (rows stored with
    /// role='note', where `message_id` is the note path and `ts` is the file
    /// mtime). Lets the note reindexer skip unchanged files and re-embed changed
    /// ones (incremental). Notes share the `chat_embeddings` table so a single
    /// cosine scan covers the whole brain (chats + notes) for unified RAG.
    pub fn note_mtimes(&self, model: &str) -> Vec<(String, i64)> {
        let conn = self.inner.lock();
        let stmt = conn.prepare(
            "SELECT message_id, ts FROM chat_embeddings WHERE model = ?1 AND role = 'note'",
        );
        let mut stmt = match stmt {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map(params![model], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)));
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// How many messages are embedded for `model`.
    pub fn chat_embedding_count(&self, model: &str) -> i64 {
        let conn = self.inner.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM chat_embeddings WHERE model = ?1",
            params![model],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
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

    /// Aggregate `agent.run` spans (optionally since `since_ms`) into a
    /// reliability report grouped by provider and by model. Pure read-side; no
    /// writes, no schema change. Percentiles are computed in Rust from the
    /// finished-run duration list (avoids SQLite percentile gymnastics).
    pub fn reliability_summary(&self, since_ms: Option<i64>) -> anyhow::Result<ReliabilityReport> {
        let recs = {
            let conn = self.inner.lock();
            let mut stmt = conn.prepare(
                "SELECT s.id, s.session_id, s.agent_id,
                        json_extract(s.attributes, '$.model') AS model,
                        s.started_at, s.ended_at, s.status,
                        COALESCE(SUM(CASE WHEN e.name = 'done'
                            THEN CAST(json_extract(e.payload, '$.tokens') AS INTEGER)
                            ELSE 0 END), 0) AS tokens,
                        MAX(CASE WHEN e.name = 'error' THEN 1 ELSE 0 END) AS had_error,
                        (SELECT json_extract(e2.payload, '$.message') FROM events e2
                           WHERE e2.span_id = s.id AND e2.name = 'error'
                           ORDER BY e2.ts DESC LIMIT 1) AS err_msg
                 FROM spans s LEFT JOIN events e ON e.span_id = s.id
                 WHERE s.name = 'agent.run' AND (?1 IS NULL OR s.started_at >= ?1)
                 GROUP BY s.id",
            )?;
            let rows = stmt.query_map(params![since_ms], |r| {
                Ok(RunRecord {
                    span_id: r.get(0)?,
                    session_id: r.get(1)?,
                    agent_id: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "unknown".into()),
                    model: r.get::<_, Option<String>>(3)?,
                    started: r.get(4)?,
                    ended: r.get::<_, Option<i64>>(5)?,
                    status: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "running".into()),
                    tokens: r.get::<_, i64>(7)?.max(0) as u64,
                    had_error: r.get::<_, i64>(8)? != 0,
                    err_msg: r.get::<_, Option<String>>(9)?,
                })
            })?;
            rows.flatten().collect::<Vec<RunRecord>>()
        };

        let mut by_provider: std::collections::BTreeMap<String, Vec<&RunRecord>> = Default::default();
        let mut by_model: std::collections::BTreeMap<String, Vec<&RunRecord>> = Default::default();
        for rec in &recs {
            by_provider.entry(rec.agent_id.clone()).or_default().push(rec);
            if let Some(m) = &rec.model {
                by_model.entry(m.clone()).or_default().push(rec);
            }
        }

        let provider_rows: Vec<ReliabilityRow> = by_provider
            .into_iter()
            .map(|(k, v)| build_reliability_row(k.clone(), Some(k), None, &v))
            .collect();
        let model_rows: Vec<ReliabilityRow> = by_model
            .into_iter()
            .map(|(k, v)| build_reliability_row(k.clone(), None, Some(k), &v))
            .collect();
        let all: Vec<&RunRecord> = recs.iter().collect();
        let totals = build_reliability_row("all".to_string(), None, None, &all);
        let mcp_tools = self.mcp_tools_summary(since_ms)?;

        Ok(ReliabilityReport {
            since_ms,
            generated_ms: chrono::Utc::now().timestamp_millis(),
            totals,
            by_provider: sort_rows(provider_rows),
            by_model: sort_rows(model_rows),
            mcp_tools,
        })
    }

    /// Aggregate MCP tool-call activity (issue 008 full scope) into
    /// [`McpToolsSummary`]: overall count/latency/cost plus a per-tool
    /// breakdown, drawn from `tool_result` events whose qualified name carries
    /// the `mcp__` prefix (see `mcp::chat_tools::MCP_TOOL_PREFIX`). Windowed
    /// the same way as [`Self::reliability_summary`] (`e.ts >= since_ms`, the
    /// event's own timestamp — a tool call can outlive its run's start).
    fn mcp_tools_summary(&self, since_ms: Option<i64>) -> anyhow::Result<McpToolsSummary> {
        struct ToolResultRow {
            span_id: String,
            name: String,
            ok: bool,
            duration_ms: Option<i64>,
        }
        let rows = {
            let conn = self.inner.lock();
            let mut stmt = conn.prepare(
                "SELECT e.span_id,
                        json_extract(e.payload, '$.name') AS tool_name,
                        json_extract(e.payload, '$.ok') AS ok,
                        CAST(json_extract(e.payload, '$.duration_ms') AS INTEGER) AS duration_ms
                 FROM events e
                 WHERE e.name = 'tool_result'
                   AND json_extract(e.payload, '$.name') LIKE 'mcp\\_\\_%' ESCAPE '\\'
                   AND (?1 IS NULL OR e.ts >= ?1)",
            )?;
            let mapped = stmt.query_map(params![since_ms], |r| {
                Ok(ToolResultRow {
                    span_id: r.get(0)?,
                    name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ok: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                    duration_ms: r.get(3)?,
                })
            })?;
            mapped.flatten().collect::<Vec<_>>()
        };

        if rows.is_empty() {
            return Ok(McpToolsSummary {
                calls: 0,
                ok_calls: 0,
                avg_ms: None,
                p95_ms: None,
                est_usd: 0.0,
                by_tool: Vec::new(),
            });
        }

        // Whole-run cost of every DISTINCT span that made at least one MCP
        // tool call — summed once per span (not per call) so a chatty run
        // with many tool calls doesn't inflate the estimate.
        let span_ids: std::collections::BTreeSet<&str> =
            rows.iter().map(|r| r.span_id.as_str()).collect();
        let est_usd = {
            use crate::pricing::{compute_usd, lookup_price, split_tokens};
            let conn = self.inner.lock();
            let mut cost = 0.0f64;
            for span_id in span_ids {
                let row: Option<(Option<String>, Option<String>, i64)> = conn
                    .query_row(
                        "SELECT s.agent_id, json_extract(s.attributes, '$.model'),
                                COALESCE((SELECT SUM(CAST(json_extract(e2.payload, '$.tokens') AS INTEGER))
                                          FROM events e2 WHERE e2.span_id = s.id AND e2.name = 'done'), 0)
                         FROM spans s WHERE s.id = ?1",
                        params![span_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok();
                if let Some((agent_id, model, tokens)) = row {
                    if tokens > 0 {
                        let key = model.or(agent_id).unwrap_or_default();
                        let price = lookup_price(&key);
                        let (p, c) = split_tokens(tokens.max(0) as u64);
                        cost += compute_usd(p, c, price);
                    }
                }
            }
            cost
        };

        let mut by_tool: std::collections::BTreeMap<String, Vec<&ToolResultRow>> = Default::default();
        for r in &rows {
            by_tool.entry(r.name.clone()).or_default().push(r);
        }
        let tool_stats = |group: &[&ToolResultRow]| -> (u64, u64, Option<i64>, Option<i64>) {
            let calls = group.len() as u64;
            let ok_calls = group.iter().filter(|r| r.ok).count() as u64;
            let mut durations: Vec<i64> = group.iter().filter_map(|r| r.duration_ms).collect();
            durations.sort_unstable();
            let avg_ms = if durations.is_empty() {
                None
            } else {
                Some(durations.iter().sum::<i64>() / durations.len() as i64)
            };
            (calls, ok_calls, avg_ms, percentile(&durations, 95.0))
        };

        let mut by_tool_rows: Vec<McpToolStat> = by_tool
            .into_iter()
            .map(|(name, group)| {
                let (calls, ok_calls, avg_ms, p95_ms) = tool_stats(&group);
                McpToolStat { name, calls, ok_calls, avg_ms, p95_ms }
            })
            .collect();
        by_tool_rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));

        let all_refs: Vec<&ToolResultRow> = rows.iter().collect();
        let (calls, ok_calls, avg_ms, p95_ms) = tool_stats(&all_refs);

        Ok(McpToolsSummary { calls, ok_calls, avg_ms, p95_ms, est_usd, by_tool: by_tool_rows })
    }

    /// Provider-level reliability rows for the cost-per-success router
    /// (issue 006): the `by_provider` slice of [`Self::reliability_summary`],
    /// windowed to `since_ms`. Read-only; a thin convenience so the routing
    /// call site doesn't unpack a whole report for the one grouping it reads.
    pub fn provider_reliability(
        &self,
        since_ms: Option<i64>,
    ) -> anyhow::Result<Vec<ReliabilityRow>> {
        Ok(self.reliability_summary(since_ms)?.by_provider)
    }

    /// Total estimated spend (across every provider/model) for one session's
    /// `agent.run` spans, over the session's entire lifetime — a budget cap
    /// (issue 006 full scope) compares against all-time spend, not a rolling
    /// window. Read-only; reuses the same per-run token→price accounting as
    /// [`Self::reliability_summary`] via [`build_reliability_row`] so the
    /// figure a session sees here always matches what Reliability/Usage would
    /// compute for the same runs.
    pub fn session_spend_usd(&self, session_id: &str) -> anyhow::Result<f64> {
        let recs = {
            let conn = self.inner.lock();
            let mut stmt = conn.prepare(
                "SELECT s.id, s.session_id, s.agent_id,
                        json_extract(s.attributes, '$.model') AS model,
                        s.started_at, s.ended_at, s.status,
                        COALESCE(SUM(CASE WHEN e.name = 'done'
                            THEN CAST(json_extract(e.payload, '$.tokens') AS INTEGER)
                            ELSE 0 END), 0) AS tokens,
                        MAX(CASE WHEN e.name = 'error' THEN 1 ELSE 0 END) AS had_error,
                        (SELECT json_extract(e2.payload, '$.message') FROM events e2
                           WHERE e2.span_id = s.id AND e2.name = 'error'
                           ORDER BY e2.ts DESC LIMIT 1) AS err_msg
                 FROM spans s LEFT JOIN events e ON e.span_id = s.id
                 WHERE s.name = 'agent.run' AND s.session_id = ?1
                 GROUP BY s.id",
            )?;
            let rows = stmt.query_map(params![session_id], |r| {
                Ok(RunRecord {
                    span_id: r.get(0)?,
                    session_id: r.get(1)?,
                    agent_id: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "unknown".into()),
                    model: r.get::<_, Option<String>>(3)?,
                    started: r.get(4)?,
                    ended: r.get::<_, Option<i64>>(5)?,
                    status: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "running".into()),
                    tokens: r.get::<_, i64>(7)?.max(0) as u64,
                    had_error: r.get::<_, i64>(8)? != 0,
                    err_msg: r.get::<_, Option<String>>(9)?,
                })
            })?;
            rows.flatten().collect::<Vec<RunRecord>>()
        };
        let refs: Vec<&RunRecord> = recs.iter().collect();
        Ok(build_reliability_row("session".to_string(), None, None, &refs).est_usd)
    }

    /// Recent `agent.run` spans for the Run Replay picker, newest first,
    /// optionally scoped to one session. Read-only.
    pub fn list_replay_runs(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ReplayRunSummary>> {
        let conn = self.inner.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.session_id, s.trace_id, s.agent_id,
                    json_extract(s.attributes, '$.model'),
                    s.started_at, s.ended_at, s.status,
                    COALESCE(SUM(CASE WHEN e.name = 'done'
                        THEN CAST(json_extract(e.payload, '$.tokens') AS INTEGER)
                        ELSE 0 END), 0) AS tokens,
                    MAX(CASE WHEN e.name = 'error' THEN 1 ELSE 0 END) AS had_error,
                    (SELECT json_extract(c.attributes, '$.first_message_preview')
                       FROM spans c WHERE c.trace_id = s.trace_id AND c.name = 'chat.turn'
                       LIMIT 1) AS prompt
             FROM spans s LEFT JOIN events e ON e.span_id = s.id
             WHERE s.name = 'agent.run' AND (?1 IS NULL OR s.session_id = ?1)
             GROUP BY s.id
             ORDER BY s.started_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |r| {
            let prompt: Option<String> = r.get::<_, Option<String>>(10)?;
            Ok(ReplayRunSummary {
                span_id: r.get(0)?,
                session_id: r.get(1)?,
                trace_id: r.get(2)?,
                agent_id: r.get::<_, Option<String>>(3)?,
                model: r.get::<_, Option<String>>(4)?,
                started_at: r.get(5)?,
                ended_at: r.get::<_, Option<i64>>(6)?,
                status: r.get::<_, Option<String>>(7)?.unwrap_or_else(|| "running".into()),
                tokens: r.get::<_, i64>(8)?.max(0) as u64,
                had_error: r.get::<_, i64>(9)? != 0,
                // Redact the prompt preview — captured raw, may contain secrets.
                prompt_preview: prompt.map(|p| crate::redact::redact_text(&p)),
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// Full replay of one run: its `agent.run` span, ordered (already-redacted)
    /// events, the routing reason + prompt preview from the trace's `chat.turn`
    /// span, and a read-time cost estimate. Read-only.
    pub fn run_replay(&self, span_id: &str) -> anyhow::Result<RunReplay> {
        use crate::pricing::{compute_usd, lookup_price, split_tokens};
        let conn = self.inner.lock();
        let (session_id, trace_id, agent_id, model, status, started_at, ended_at) = conn
            .query_row(
                "SELECT session_id, trace_id, agent_id, json_extract(attributes, '$.model'),
                        status, started_at, ended_at
                 FROM spans WHERE id = ?1 AND name = 'agent.run'",
                params![span_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?.unwrap_or_else(|| "running".into()),
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .map_err(|_| anyhow::anyhow!("run not found: {span_id}"))?;

        // Routing reason + prompt preview live on the trace's chat.turn span.
        let (routing_reason, prompt_preview): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT json_extract(attributes, '$.routing_reason'),
                        json_extract(attributes, '$.first_message_preview')
                 FROM spans WHERE trace_id = ?1 AND name = 'chat.turn' LIMIT 1",
                params![trace_id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .unwrap_or((None, None));
        // routing_reason was redacted at write; prompt_preview was not.
        let prompt_preview = prompt_preview.map(|p| crate::redact::redact_text(&p));

        let mut stmt = conn.prepare(
            "SELECT ts, name, payload FROM events WHERE span_id = ?1 ORDER BY ts ASC, rowid ASC",
        )?;
        let steps: Vec<ReplayStepRow> = stmt
            .query_map(params![span_id], |r| {
                let payload_str: String = r.get(2)?;
                let payload: serde_json::Value =
                    serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);
                Ok(ReplayStepRow { ts: r.get(0)?, name: r.get(1)?, payload })
            })?
            .flatten()
            .collect();

        // Read-time token + cost from the `done` step(s).
        let total_tokens: u64 = steps
            .iter()
            .filter(|s| s.name == "done")
            .filter_map(|s| s.payload.get("tokens").and_then(|t| t.as_u64()))
            .sum();
        let est_usd = if total_tokens > 0 {
            let price = lookup_price(model.as_deref().unwrap_or(agent_id.as_deref().unwrap_or("")));
            let (p, c) = split_tokens(total_tokens);
            compute_usd(p, c, price)
        } else {
            0.0
        };

        Ok(RunReplay {
            span_id: span_id.to_string(),
            session_id,
            trace_id,
            agent_id,
            model,
            status,
            started_at,
            ended_at,
            routing_reason,
            prompt_preview,
            total_tokens,
            est_usd,
            steps,
        })
    }

    /// Serialize a run replay as redacted JSONL (one metadata line + one line
    /// per step). Every line is passed through `redact::redact_text` on the way
    /// out — a single export choke-point — so no API key / secret / obvious
    /// credential can leave the machine even if a raw one reached the store.
    pub fn export_run_replay_jsonl(&self, span_id: &str) -> anyhow::Result<String> {
        let replay = self.run_replay(span_id)?;
        let mut out = String::new();
        let meta = serde_json::json!({
            "kind": "run_replay_meta",
            "span_id": replay.span_id,
            "session_id": replay.session_id,
            "agent_id": replay.agent_id,
            "model": replay.model,
            "status": replay.status,
            "started_at": replay.started_at,
            "ended_at": replay.ended_at,
            "routing_reason": replay.routing_reason,
            "prompt_preview": replay.prompt_preview,
            "total_tokens": replay.total_tokens,
            "est_usd": replay.est_usd,
        });
        out.push_str(&crate::redact::redact_text(&meta.to_string()));
        out.push('\n');
        for step in &replay.steps {
            let line = serde_json::to_string(step).unwrap_or_default();
            out.push_str(&crate::redact::redact_text(&line));
            out.push('\n');
        }
        Ok(out)
    }
}

/// Sort reliability rows by run count desc (busiest first), stable on key.
fn sort_rows(mut rows: Vec<ReliabilityRow>) -> Vec<ReliabilityRow> {
    rows.sort_by(|a, b| b.runs.cmp(&a.runs).then_with(|| a.key.cmp(&b.key)));
    rows
}

/// Nearest-rank percentile over an already-sorted ascending slice.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

/// Reduce a group of run records into one dashboard row. A run counts as an
/// error if it emitted an `error` event OR its span status is `error`; as ok if
/// it finished (has `ended_at`) and is not an error; otherwise it is still
/// running/stale and excluded from the success denominator.
fn build_reliability_row(
    key: String,
    agent_id: Option<String>,
    model: Option<String>,
    recs: &[&RunRecord],
) -> ReliabilityRow {
    use crate::pricing::{compute_usd, lookup_price, split_tokens};
    let mut ok_runs = 0u64;
    let mut error_runs = 0u64;
    let mut running_runs = 0u64;
    let mut total_tokens = 0u64;
    let mut est_usd = 0.0f64;
    let mut durations: Vec<i64> = Vec::new();
    let mut last_run_ms = 0i64;
    let mut error_class_counts: std::collections::HashMap<String, u64> = Default::default();
    let mut last_error: Option<(i64, String, String)> = None; // (started, span_id, session_id)

    for rec in recs {
        let is_error = rec.had_error || rec.status == "error";
        if is_error {
            error_runs += 1;
            let class = error_class(rec.err_msg.as_deref().unwrap_or("agent error"));
            *error_class_counts.entry(class).or_insert(0) += 1;
            if last_error.as_ref().map(|(t, ..)| rec.started > *t).unwrap_or(true) {
                last_error = Some((rec.started, rec.span_id.clone(), rec.session_id.clone()));
            }
        } else if rec.ended.is_some() {
            ok_runs += 1;
        } else {
            running_runs += 1;
        }
        if let Some(ended) = rec.ended {
            let d = (ended - rec.started).max(0);
            durations.push(d);
        }
        total_tokens += rec.tokens;
        if rec.tokens > 0 {
            let price = lookup_price(rec.model.as_deref().unwrap_or(&rec.agent_id));
            let (p, c) = split_tokens(rec.tokens);
            est_usd += compute_usd(p, c, price);
        }
        last_run_ms = last_run_ms.max(rec.started);
    }

    durations.sort_unstable();
    let avg_ms = if durations.is_empty() {
        None
    } else {
        Some((durations.iter().sum::<i64>() / durations.len() as i64).max(0))
    };
    let finished = ok_runs + error_runs;
    let success_rate = if finished > 0 { ok_runs as f64 / finished as f64 } else { 0.0 };
    let top_error_class = error_class_counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c);

    ReliabilityRow {
        key,
        agent_id,
        model,
        runs: recs.len() as u64,
        ok_runs,
        error_runs,
        running_runs,
        success_rate,
        p50_ms: percentile(&durations, 50.0),
        p95_ms: percentile(&durations, 95.0),
        avg_ms,
        total_tokens,
        est_usd,
        top_error_class,
        last_run_ms,
        last_error_span: last_error.as_ref().map(|(_, s, _)| s.clone()),
        last_error_session: last_error.as_ref().map(|(_, _, sess)| sess.clone()),
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

    /// Reliability aggregation: ok/error classification, success rate,
    /// percentiles, token totals, and grouping by provider + model.
    #[test]
    fn reliability_summary_classifies_and_aggregates() {
        let store = TracingStore::in_memory();
        // Two ok runs and one errored run on the same provider/model.
        for (i, tokens) in [(0, 100u64), (1, 60u64)].iter() {
            let sid = format!("ok-{i}");
            store
                .start_agent_run(&sid, "t", "sess", "gateway-remote", Some("claude-sonnet-4-6"))
                .unwrap();
            store
                .record_event(&sid, &AgentEvent::Done { total_tokens: Some(*tokens), run_id: None })
                .unwrap();
            store.finish_agent_run(&sid).unwrap();
        }
        store
            .start_agent_run("err-1", "t", "sess", "gateway-remote", Some("claude-sonnet-4-6"))
            .unwrap();
        store
            .record_event("err-1", &AgentEvent::Error { message: "upstream returned 500".into() })
            .unwrap();
        store.finish_agent_run("err-1").unwrap();

        let report = store.reliability_summary(None).unwrap();
        assert_eq!(report.totals.runs, 3);
        assert_eq!(report.totals.ok_runs, 2);
        assert_eq!(report.totals.error_runs, 1);
        assert!((report.totals.success_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(report.totals.total_tokens, 160);
        assert!(report.totals.est_usd > 0.0, "cost estimated from tokens");
        assert_eq!(report.totals.top_error_class.as_deref(), Some("UpstreamError"));

        // Grouped views: one provider row, one model row, same counts.
        let prov = report.by_provider.iter().find(|r| r.key == "gateway-remote").unwrap();
        assert_eq!(prov.runs, 3);
        assert_eq!(prov.error_runs, 1);
        let model = report.by_model.iter().find(|r| r.key == "claude-sonnet-4-6").unwrap();
        assert_eq!(model.runs, 3);
        assert!(model.p95_ms.is_some(), "percentiles computed from finished runs");

        // `since_ms` in the future excludes everything → empty, not an error.
        let empty = store
            .reliability_summary(Some(chrono::Utc::now().timestamp_millis() + 60_000))
            .unwrap();
        assert_eq!(empty.totals.runs, 0);
        assert_eq!(empty.totals.success_rate, 0.0);

        // The failing row deep-link fields point at the errored run.
        let prov = store.reliability_summary(None).unwrap();
        let row = prov.by_provider.iter().find(|r| r.key == "gateway-remote").unwrap();
        assert_eq!(row.last_error_span.as_deref(), Some("err-1"));

        // A report with zero MCP tool activity carries a quiet, zeroed
        // summary — never an error or a missing field.
        assert_eq!(report.mcp_tools.calls, 0);
        assert_eq!(report.mcp_tools.est_usd, 0.0);
        assert!(report.mcp_tools.by_tool.is_empty());
    }

    /// Issue 008 full scope: MCP tool-call count/latency aggregate into
    /// `reliability_summary`'s `mcp_tools` over synthetic tool_call/
    /// tool_result events — count, ok/error split, avg/p95 latency, a
    /// per-tool breakdown, and a run-level cost estimate for runs that made
    /// at least one MCP call. Non-MCP tool calls (no `mcp__` prefix) must not
    /// be swept in.
    #[test]
    fn reliability_summary_aggregates_mcp_tool_calls() {
        let store = TracingStore::in_memory();

        // Run A: two calls to the same tool, one ok (120ms) one failed (80ms),
        // plus one call to a second tool (200ms) — all on a priced model.
        store
            .start_agent_run("mcp-a", "t", "sess", "ollama", Some("claude-sonnet-4-6"))
            .unwrap();
        store
            .record_event(
                "mcp-a",
                &AgentEvent::ToolResult {
                    name: "mcp__fs__read_file".into(),
                    ok: true,
                    summary: "ok".into(),
                    duration_ms: Some(120),
                },
            )
            .unwrap();
        store
            .record_event(
                "mcp-a",
                &AgentEvent::ToolResult {
                    name: "mcp__fs__read_file".into(),
                    ok: false,
                    summary: "denied".into(),
                    duration_ms: Some(80),
                },
            )
            .unwrap();
        store
            .record_event(
                "mcp-a",
                &AgentEvent::ToolResult {
                    name: "mcp__weather__get_forecast".into(),
                    ok: true,
                    summary: "sunny".into(),
                    duration_ms: Some(200),
                },
            )
            .unwrap();
        store
            .record_event("mcp-a", &AgentEvent::Done { total_tokens: Some(100), run_id: None })
            .unwrap();
        store.finish_agent_run("mcp-a").unwrap();

        // Run B: a non-MCP tool call — must not be counted at all.
        store
            .start_agent_run("plain-b", "t", "sess", "ollama", Some("claude-sonnet-4-6"))
            .unwrap();
        store
            .record_event(
                "plain-b",
                &AgentEvent::ToolResult {
                    name: "update_focus_chain".into(),
                    ok: true,
                    summary: "ok".into(),
                    duration_ms: Some(5),
                },
            )
            .unwrap();
        store
            .record_event("plain-b", &AgentEvent::Done { total_tokens: Some(50), run_id: None })
            .unwrap();
        store.finish_agent_run("plain-b").unwrap();

        let report = store.reliability_summary(None).unwrap();
        let mcp = &report.mcp_tools;
        assert_eq!(mcp.calls, 3, "3 MCP calls total, the plain tool excluded");
        assert_eq!(mcp.ok_calls, 2);
        assert_eq!(mcp.avg_ms, Some((120 + 80 + 200) / 3));
        assert!(mcp.p95_ms.is_some());
        assert!(mcp.est_usd > 0.0, "run-a's priced tokens attribute a cost");

        assert_eq!(mcp.by_tool.len(), 2, "two distinct MCP tools");
        let read_file = mcp.by_tool.iter().find(|t| t.name == "mcp__fs__read_file").unwrap();
        assert_eq!(read_file.calls, 2);
        assert_eq!(read_file.ok_calls, 1);
        assert_eq!(read_file.avg_ms, Some((120 + 80) / 2));
        let forecast = mcp
            .by_tool
            .iter()
            .find(|t| t.name == "mcp__weather__get_forecast")
            .unwrap();
        assert_eq!(forecast.calls, 1);
        assert_eq!(forecast.ok_calls, 1);

        // `since_ms` windowing applies to MCP aggregation too.
        let future = store
            .reliability_summary(Some(chrono::Utc::now().timestamp_millis() + 60_000))
            .unwrap();
        assert_eq!(future.mcp_tools.calls, 0);
    }

    /// `session_spend_usd` (issue 006 full scope — budget ceilings): scoped to
    /// one session's `agent.run` spans, ignores runs in other sessions, and a
    /// session with no runs at all is `0.0`, not an error.
    #[test]
    fn session_spend_usd_scopes_to_one_session() {
        let store = TracingStore::in_memory();
        for (i, tokens) in [(0, 100u64), (1, 60u64)].iter() {
            let sid = format!("budget-ok-{i}");
            store
                .start_agent_run(&sid, "t", "session-a", "gateway-remote", Some("claude-sonnet-4-6"))
                .unwrap();
            store
                .record_event(&sid, &AgentEvent::Done { total_tokens: Some(*tokens), run_id: None })
                .unwrap();
            store.finish_agent_run(&sid).unwrap();
        }
        // A run in a different session must not count toward session-a's spend.
        store
            .start_agent_run("budget-other", "t", "session-b", "gateway-remote", Some("claude-sonnet-4-6"))
            .unwrap();
        store
            .record_event("budget-other", &AgentEvent::Done { total_tokens: Some(1_000_000), run_id: None })
            .unwrap();
        store.finish_agent_run("budget-other").unwrap();

        let spend_a = store.session_spend_usd("session-a").unwrap();
        let spend_b = store.session_spend_usd("session-b").unwrap();
        assert!(spend_a > 0.0, "session-a spent something");
        assert!(spend_b > spend_a, "session-b's single huge run costs more");

        // Matches the same per-run pricing reliability_summary would compute
        // for just session-a's runs.
        let report = store.reliability_summary(None).unwrap();
        assert!((report.totals.est_usd - (spend_a + spend_b)).abs() < 1e-9);

        // A session with no runs at all is a quiet 0.0, not an error.
        assert_eq!(store.session_spend_usd("session-never-existed").unwrap(), 0.0);
    }

    /// Run Replay: the timeline reconstructs a run's ordered events, and the
    /// JSONL export redacts secrets even when a raw one reached the store.
    #[test]
    fn run_replay_timeline_and_redacted_export() {
        let store = TracingStore::in_memory();
        // A chat.turn carrying a routing reason + prompt preview for the trace.
        store
            .record_chat_turn("trace-r", "sess-r", "please refactor auth.rs", &["gateway-remote".into()], Some("explicit pick: gateway-remote"))
            .unwrap();
        store
            .start_agent_run("run-r", "trace-r", "sess-r", "gateway-remote", Some("claude-sonnet-4-6"))
            .unwrap();
        store
            .record_event("run-r", &AgentEvent::ToolCall { name: "read_file".into(), args: serde_json::Value::Null, preview: Some("auth.rs".into()) })
            .unwrap();
        // A poisoned error message reaches the store RAW (record_event does not
        // itself redact — the chat path redacts before it; the export must too).
        store
            .record_event("run-r", &AgentEvent::Error { message: "boom key=sk-ant-api03-DEADBEEFdeadbeef0123456789 leaked".into() })
            .unwrap();
        store
            .record_event("run-r", &AgentEvent::Done { total_tokens: Some(120), run_id: None })
            .unwrap();
        store.finish_agent_run("run-r").unwrap();

        let replay = store.run_replay("run-r").unwrap();
        assert_eq!(replay.span_id, "run-r");
        assert_eq!(replay.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(replay.routing_reason.as_deref(), Some("explicit pick: gateway-remote"));
        assert_eq!(replay.total_tokens, 120);
        assert!(replay.est_usd > 0.0);
        // Ordered: tool_call → error → done.
        let names: Vec<&str> = replay.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["tool_call", "error", "done"]);

        // The picker lists this run.
        let runs = store.list_replay_runs(Some("sess-r"), 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].span_id, "run-r");
        assert!(runs[0].had_error);

        // Export redacts the leaked key — the choke-point guarantee.
        let jsonl = store.export_run_replay_jsonl("run-r").unwrap();
        assert!(!jsonl.contains("sk-ant-api03-DEADBEEF"), "secret must not survive export");
        assert!(jsonl.contains("run_replay_meta"));
        assert!(jsonl.contains("tool_call"));
    }
}
