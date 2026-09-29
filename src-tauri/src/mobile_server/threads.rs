//! v2 threads = chat sessions. A thread id IS the session id keyed in the
//! trace store's `messages` table (what the desktop persists every turn into
//! via `record_message`), so desktop + phone list the same conversations.
//!
//! The desktop additionally keeps per-project "thread lanes" as JSON files
//! (`commands::threads`, `<root>/.cortex/threads/*.json`) with a
//! frontend-owned message schema; those are NOT read here — they wrap the
//! same session ids, so a lane's conversation still shows up through its
//! session's messages. The small `v2_threads` table below holds what the
//! `messages` rows can't: a thread that exists before its first message, a
//! user-set title, and the agent/model defaults chosen on the phone. It is
//! created lazily on the store's connection (no `schema.sql` change).

use rusqlite::{params, OptionalExtension};

use super::events::{preview, MessageView, ThreadView, V2Hub, THREAD_PREVIEW_CHARS};
use crate::observability::tracing_store::TracingStore;

const CREATE: &str = "CREATE TABLE IF NOT EXISTS v2_threads (
    id           TEXT PRIMARY KEY,
    title        TEXT,
    project_root TEXT,
    agent_id     TEXT,
    model        TEXT,
    created_ms   INTEGER NOT NULL,
    last_ms      INTEGER NOT NULL
)";

/// Default page size / hard ceiling for list endpoints.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
pub const MAX_MESSAGES: usize = 500;

/// Create the metadata table (idempotent; called once at server start).
pub fn ensure_schema(store: &TracingStore) {
    let conn = store.shared_connection();
    let conn = conn.lock();
    if let Err(e) = conn.execute(CREATE, []) {
        tracing::warn!("v2_threads schema: {e}");
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadMeta {
    pub id: String,
    pub title: Option<String>,
    pub project_root: Option<String>,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub created_ms: i64,
    pub last_ms: i64,
}

pub fn upsert_meta(store: &TracingStore, m: &ThreadMeta) -> Result<(), String> {
    let conn = store.shared_connection();
    let conn = conn.lock();
    conn.execute(
        "INSERT INTO v2_threads (id, title, project_root, agent_id, model, created_ms, last_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
           title = COALESCE(excluded.title, title),
           project_root = COALESCE(excluded.project_root, project_root),
           agent_id = COALESCE(excluded.agent_id, agent_id),
           model = COALESCE(excluded.model, model),
           last_ms = MAX(last_ms, excluded.last_ms)",
        params![
            m.id,
            m.title,
            m.project_root,
            m.agent_id,
            m.model,
            m.created_ms,
            m.last_ms
        ],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub fn get_meta(store: &TracingStore, id: &str) -> Option<ThreadMeta> {
    let conn = store.shared_connection();
    let conn = conn.lock();
    conn.query_row(
        "SELECT id, title, project_root, agent_id, model, created_ms, last_ms
         FROM v2_threads WHERE id = ?1",
        params![id],
        |r| {
            Ok(ThreadMeta {
                id: r.get(0)?,
                title: r.get(1)?,
                project_root: r.get(2)?,
                agent_id: r.get(3)?,
                model: r.get(4)?,
                created_ms: r.get(5)?,
                last_ms: r.get(6)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Rename. Creates the metadata row when the thread only exists as messages.
pub fn set_title(store: &TracingStore, id: &str, title: &str) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    let existing = get_meta(store, id);
    upsert_meta(
        store,
        &ThreadMeta {
            id: id.to_string(),
            title: Some(title.to_string()),
            created_ms: existing.as_ref().map(|m| m.created_ms).unwrap_or(now),
            last_ms: existing.as_ref().map(|m| m.last_ms).unwrap_or(now),
            ..Default::default()
        },
    )?;
    // COALESCE keeps the old title when the new one is NULL — but a rename
    // must always win, so set it explicitly too.
    let conn = store.shared_connection();
    let conn = conn.lock();
    conn.execute(
        "UPDATE v2_threads SET title = ?2 WHERE id = ?1",
        params![id, title],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Delete the thread: its metadata AND its messages (the desktop's recent
/// sessions list loses it too — one set of conversations).
pub fn delete_thread(store: &TracingStore, id: &str) -> Result<bool, String> {
    let had_meta = {
        let conn = store.shared_connection();
        let conn = conn.lock();
        conn.execute("DELETE FROM v2_threads WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?
            > 0
    };
    let n = store
        .delete_session_messages(id)
        .map_err(|e| e.to_string())?;
    Ok(had_meta || n > 0)
}

/// Does a thread exist (metadata row or at least one message)?
pub fn exists(store: &TracingStore, id: &str) -> bool {
    if get_meta(store, id).is_some() {
        return true;
    }
    let conn = store.shared_connection();
    let conn = conn.lock();
    conn.query_row(
        "SELECT 1 FROM messages WHERE session_id = ?1 LIMIT 1",
        params![id],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Bump `last_ms` on the metadata row (no-op when there is none).
pub fn touch(store: &TracingStore, id: &str, last_ms: i64) {
    let conn = store.shared_connection();
    let conn = conn.lock();
    let _ = conn.execute(
        "UPDATE v2_threads SET last_ms = MAX(last_ms, ?2) WHERE id = ?1",
        params![id, last_ms],
    );
}

/// Aggregate of a session's `messages` rows.
#[derive(Debug, Clone, Default)]
struct SessionAgg {
    first_ms: i64,
    last_ms: i64,
    count: i64,
}

fn session_aggs(store: &TracingStore) -> Vec<(String, SessionAgg)> {
    let conn = store.shared_connection();
    let conn = conn.lock();
    let Ok(mut stmt) = conn
        .prepare("SELECT session_id, MIN(ts), MAX(ts), COUNT(*) FROM messages GROUP BY session_id")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            SessionAgg {
                first_ms: r.get(1)?,
                last_ms: r.get(2)?,
                count: r.get(3)?,
            },
        ))
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

fn all_meta(store: &TracingStore) -> Vec<ThreadMeta> {
    let conn = store.shared_connection();
    let conn = conn.lock();
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, title, project_root, agent_id, model, created_ms, last_ms FROM v2_threads",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(ThreadMeta {
            id: r.get(0)?,
            title: r.get(1)?,
            project_root: r.get(2)?,
            agent_id: r.get(3)?,
            model: r.get(4)?,
            created_ms: r.get(5)?,
            last_ms: r.get(6)?,
        })
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// `(first user message, latest message content, latest project_root)`.
fn session_details(
    store: &TracingStore,
    id: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let conn = store.shared_connection();
    let conn = conn.lock();
    let first_user: Option<String> = conn
        .query_row(
            "SELECT content FROM messages WHERE session_id = ?1 AND role = 'user'
             ORDER BY ts ASC LIMIT 1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let latest: Option<String> = conn
        .query_row(
            "SELECT content FROM messages WHERE session_id = ?1 ORDER BY ts DESC LIMIT 1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let project: Option<String> = conn
        .query_row(
            "SELECT project_root FROM messages
             WHERE session_id = ?1 AND project_root IS NOT NULL AND project_root != ''
             ORDER BY ts DESC LIMIT 1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    (first_user, latest, project)
}

/// Build the wire view for one thread candidate.
fn build_view(
    store: &TracingStore,
    hub: &V2Hub,
    id: &str,
    meta: Option<&ThreadMeta>,
    agg: Option<&SessionAgg>,
) -> ThreadView {
    let (first_user, latest, msg_project) = session_details(store, id);
    let running = hub.is_running(id);
    let live = if running { hub.live_message(id) } else { None };
    let title = meta
        .and_then(|m| m.title.clone())
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            first_user
                .as_deref()
                .map(|s| preview(s.trim(), 80))
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| "New chat".to_string());
    let last_preview = live
        .as_ref()
        .map(|m| m.content.clone())
        .filter(|c| !c.trim().is_empty())
        .or(latest)
        .map(|s| preview(s.trim(), THREAD_PREVIEW_CHARS))
        .unwrap_or_default();
    let created_ms = match (meta, agg) {
        (Some(m), Some(a)) => m.created_ms.min(a.first_ms),
        (Some(m), None) => m.created_ms,
        (None, Some(a)) => a.first_ms,
        (None, None) => 0,
    };
    let mut last_ms = match (meta, agg) {
        (Some(m), Some(a)) => m.last_ms.max(a.last_ms),
        (Some(m), None) => m.last_ms,
        (None, Some(a)) => a.last_ms,
        (None, None) => created_ms,
    };
    if let Some(m) = &live {
        last_ms = last_ms.max(m.ts_ms);
    }
    ThreadView {
        id: id.to_string(),
        title,
        project_root: meta.and_then(|m| m.project_root.clone()).or(msg_project),
        agent_id: meta
            .and_then(|m| m.agent_id.clone())
            .or_else(|| live.as_ref().and_then(|m| m.agent_id.clone())),
        model: meta.and_then(|m| m.model.clone()),
        created_ms,
        last_ms,
        pending_approvals: hub.pending_approvals_for(id),
        running,
        last_preview,
    }
}

/// One thread, or `None` when it doesn't exist.
pub fn thread_view(store: &TracingStore, hub: &V2Hub, id: &str) -> Option<ThreadView> {
    let meta = get_meta(store, id);
    let agg = session_aggs(store).into_iter().find(|(s, _)| s == id);
    if meta.is_none() && agg.is_none() && !hub.is_running(id) {
        return None;
    }
    Some(build_view(
        store,
        hub,
        id,
        meta.as_ref(),
        agg.as_ref().map(|(_, a)| a),
    ))
}

/// Paginated thread list, newest activity first. `cursor` is the `last_ms`
/// of the last row of the previous page (opaque to clients, returned as
/// `next_cursor`). `project` filters on the thread's project root.
pub fn list_threads(
    store: &TracingStore,
    hub: &V2Hub,
    project: Option<&str>,
    limit: usize,
    cursor: Option<i64>,
) -> (Vec<ThreadView>, Option<String>) {
    let limit = limit.clamp(1, MAX_LIMIT);
    let metas = all_meta(store);
    let aggs = session_aggs(store);
    // Candidate ids with a coarse `last_ms` for ordering + cursoring.
    let mut coarse: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for m in &metas {
        coarse
            .entry(m.id.clone())
            .and_modify(|v| *v = (*v).max(m.last_ms))
            .or_insert(m.last_ms);
    }
    for (id, a) in &aggs {
        coarse
            .entry(id.clone())
            .and_modify(|v| *v = (*v).max(a.last_ms))
            .or_insert(a.last_ms);
    }
    let mut ids: Vec<(String, i64)> = coarse.into_iter().collect();
    ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let project_norm = project.map(|p| p.trim_end_matches(['/', '\\']).to_string());
    let mut out: Vec<ThreadView> = Vec::new();
    let mut next_cursor: Option<String> = None;
    for (id, last) in ids {
        if cursor.is_some_and(|c| last >= c) {
            continue;
        }
        let meta = metas.iter().find(|m| m.id == id);
        let agg = aggs.iter().find(|(s, _)| *s == id).map(|(_, a)| a);
        let view = build_view(store, hub, &id, meta, agg);
        if let Some(p) = &project_norm {
            let matches = view
                .project_root
                .as_deref()
                .map(|r| r.trim_end_matches(['/', '\\']) == p)
                .unwrap_or(false);
            if !matches {
                continue;
            }
        }
        if out.len() == limit {
            next_cursor = Some(last.to_string());
            break;
        }
        out.push(view);
    }
    (out, next_cursor)
}

/// Messages of a thread, oldest first, with the in-flight assistant message
/// appended while a run is active. `before` = ts_ms exclusive upper bound
/// (older pages); `after` = a message id, returning only what came after it
/// (incremental resume).
pub fn messages(
    store: &TracingStore,
    hub: &V2Hub,
    id: &str,
    limit: usize,
    before: Option<i64>,
    after: Option<&str>,
) -> Vec<MessageView> {
    let limit = limit.clamp(1, MAX_MESSAGES);
    let after_ts: Option<i64> = after.and_then(|mid| {
        let conn = store.shared_connection();
        let conn = conn.lock();
        conn.query_row(
            "SELECT ts FROM messages WHERE id = ?1 AND session_id = ?2",
            params![mid, id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    });
    let mut rows: Vec<MessageView> = {
        let conn = store.shared_connection();
        let conn = conn.lock();
        let stmt = conn.prepare(
            "SELECT id, ts, role, agent_id, content, run_id, reasoning
             FROM messages
             WHERE session_id = ?1
               AND (?2 IS NULL OR ts < ?2)
               AND (?3 IS NULL OR ts > ?3)
             ORDER BY ts DESC, rowid DESC
             LIMIT ?4",
        );
        match stmt {
            Ok(mut stmt) => {
                let mapped = stmt.query_map(params![id, before, after_ts, limit as i64], |r| {
                    let role: String = r.get(2)?;
                    let content: String = r.get(4)?;
                    Ok(MessageView {
                        id: r.get(0)?,
                        ts_ms: r.get(1)?,
                        error: (role == "error").then(|| content.clone()),
                        role: match role.as_str() {
                            "user" | "assistant" | "system" => role.clone(),
                            _ => "system".to_string(),
                        },
                        agent_id: r.get(3)?,
                        content,
                        run_id: r.get(5)?,
                        reasoning: r.get(6)?,
                        tool_calls: None,
                        approval: None,
                        routing_reason: None,
                        pending: false,
                    })
                });
                match mapped {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(_) => Vec::new(),
                }
            }
            Err(_) => Vec::new(),
        }
    };
    rows.reverse();
    if before.is_none() {
        if let Some(live) = hub.live_message(id) {
            rows.push(live);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::tracing_store::StoredMessage;

    fn msg(session: &str, id: &str, ts: i64, role: &str, content: &str) -> StoredMessage {
        StoredMessage {
            id: id.into(),
            session_id: session.into(),
            ts,
            role: role.into(),
            agent_id: None,
            content: content.into(),
            run_id: None,
            reasoning: None,
            project_root: Some("/proj/a".into()),
        }
    }

    #[test]
    fn threads_merge_sessions_and_metadata_with_pagination() {
        let store = TracingStore::in_memory();
        ensure_schema(&store);
        let hub = V2Hub::new(store.clone());
        // Two sessions from the desktop's messages table…
        for m in [
            msg("s1", "m1", 100, "user", "First question about rust"),
            msg("s1", "m2", 200, "assistant", "An answer"),
            msg("s2", "m3", 300, "user", "Second"),
        ] {
            store.record_message(&m).unwrap();
        }
        // …one empty phone-created thread with a title + project.
        upsert_meta(
            &store,
            &ThreadMeta {
                id: "s3".into(),
                title: Some("Planning".into()),
                project_root: Some("/proj/b".into()),
                created_ms: 400,
                last_ms: 400,
                ..Default::default()
            },
        )
        .unwrap();

        let (all, next) = list_threads(&store, &hub, None, 50, None);
        assert_eq!(
            all.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["s3", "s2", "s1"]
        );
        assert!(next.is_none());
        assert_eq!(all[2].title, "First question about rust");
        assert_eq!(all[2].last_preview, "An answer");
        assert_eq!(all[2].created_ms, 100);
        assert_eq!(all[2].last_ms, 200);
        assert_eq!(all[2].project_root.as_deref(), Some("/proj/a"));
        assert_eq!(all[0].title, "Planning");
        assert_eq!(all[0].project_root.as_deref(), Some("/proj/b"));
        assert!(!all[0].running);

        // Pagination by cursor.
        let (page, next) = list_threads(&store, &hub, None, 2, None);
        assert_eq!(page.len(), 2);
        assert_eq!(next.as_deref(), Some("300"));
        let (rest, next2) = list_threads(&store, &hub, None, 2, Some(300));
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "s1");
        assert!(next2.is_none());

        // Project filter (trailing slash tolerated).
        let (a, _) = list_threads(&store, &hub, Some("/proj/a/"), 50, None);
        assert_eq!(a.len(), 2);

        // Rename + delete.
        set_title(&store, "s1", "Rust Q").unwrap();
        assert_eq!(thread_view(&store, &hub, "s1").unwrap().title, "Rust Q");
        assert!(exists(&store, "s1"));
        assert!(delete_thread(&store, "s1").unwrap());
        assert!(!exists(&store, "s1"));
        assert!(thread_view(&store, &hub, "s1").is_none());
        assert!(!delete_thread(&store, "s1").unwrap());
    }

    #[test]
    fn messages_page_and_resume_and_include_live_message() {
        let store = TracingStore::in_memory();
        ensure_schema(&store);
        let hub = V2Hub::new(store.clone());
        for i in 1..=5 {
            store
                .record_message(&msg(
                    "s",
                    &format!("m{i}"),
                    i * 10,
                    if i % 2 == 1 { "user" } else { "assistant" },
                    &format!("msg {i}"),
                ))
                .unwrap();
        }
        store
            .record_message(&msg("s", "e1", 60, "error", "boom"))
            .unwrap();
        let all = messages(&store, &hub, "s", 200, None, None);
        assert_eq!(all.len(), 6);
        assert_eq!(all[0].id, "m1");
        assert_eq!(all[5].role, "system");
        assert_eq!(all[5].error.as_deref(), Some("boom"));
        // Newest `limit` when paging, still oldest-first.
        let last2 = messages(&store, &hub, "s", 2, None, None);
        assert_eq!(
            last2.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m5", "e1"]
        );
        let older = messages(&store, &hub, "s", 2, Some(50), None);
        assert_eq!(
            older.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m3", "m4"]
        );
        let resumed = messages(&store, &hub, "s", 200, None, Some("m4"));
        assert_eq!(
            resumed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m5", "e1"]
        );
        // In-flight assistant message is appended while running.
        hub.begin_run("s", Some("claude-cli".into()), None, None);
        hub.on_chat_payload(
            "s",
            &serde_json::json!({ "agent_id": "claude-cli", "event": { "type": "token", "delta": "partial" } }),
        );
        let with_live = messages(&store, &hub, "s", 200, None, None);
        assert_eq!(with_live.len(), 7);
        assert!(with_live[6].pending);
        assert_eq!(with_live[6].content, "partial");
        let t = thread_view(&store, &hub, "s").unwrap();
        assert!(t.running);
        assert_eq!(t.last_preview, "partial");
    }
}
