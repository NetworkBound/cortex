CREATE TABLE IF NOT EXISTS spans (
  id              TEXT PRIMARY KEY,
  parent_id       TEXT,
  trace_id        TEXT NOT NULL,
  session_id      TEXT NOT NULL,
  agent_id        TEXT,
  name            TEXT NOT NULL,
  started_at      INTEGER NOT NULL,
  ended_at        INTEGER,
  status          TEXT NOT NULL,
  attributes      TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS spans_session_idx ON spans(session_id, started_at);
CREATE INDEX IF NOT EXISTS spans_trace_idx   ON spans(trace_id);

CREATE TABLE IF NOT EXISTS events (
  span_id   TEXT NOT NULL,
  ts        INTEGER NOT NULL,
  name      TEXT NOT NULL,
  payload   TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS events_span_idx ON events(span_id, ts);

CREATE TABLE IF NOT EXISTS health_samples (
  source     TEXT NOT NULL,
  ts         INTEGER NOT NULL,
  ok         INTEGER NOT NULL,
  latency_ms INTEGER,
  payload    TEXT
);
CREATE INDEX IF NOT EXISTS health_idx ON health_samples(source, ts);

CREATE TABLE IF NOT EXISTS issues (
  fingerprint  TEXT PRIMARY KEY,
  agent_id     TEXT,
  error_class  TEXT,
  -- Wave 175 — the `message` column was referenced by INSERT INTO issues
  -- in tracing_store.rs but never declared in this CREATE. Same shape of
  -- bug as wave 172's crash_log. Production impact: every issue ingest
  -- was failing with "issues has no column named message".
  message      TEXT,
  first_seen   INTEGER NOT NULL,
  last_seen    INTEGER NOT NULL,
  count        INTEGER NOT NULL DEFAULT 1,
  example_span TEXT,
  status       TEXT NOT NULL DEFAULT 'unresolved'
);

CREATE TABLE IF NOT EXISTS audit_log (
  ts        INTEGER NOT NULL,
  session_id TEXT,
  agent_id   TEXT,
  action     TEXT NOT NULL,
  detail     TEXT
);
CREATE INDEX IF NOT EXISTS audit_idx ON audit_log(ts);

-- Cortex's own chat history. Distinct from the `events` stream above
-- (which captures per-span SSE deltas). Each row is one assistant or user
-- turn as the user sees it. `load_session_messages` + `record_message`
-- in `tracing_store.rs` use this table. Without it, every chat is lost
-- the moment the user switches tabs.
CREATE TABLE IF NOT EXISTS messages (
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL,
  ts           INTEGER NOT NULL,
  role         TEXT NOT NULL,
  agent_id     TEXT,
  content      TEXT NOT NULL,
  run_id       TEXT,
  reasoning    TEXT,
  project_root TEXT
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, ts);
CREATE INDEX IF NOT EXISTS idx_messages_project ON messages(project_root, ts);


-- Wave 172 — crash_log table was referenced from INSERT/SELECT in
-- observability/crash.rs but never declared in schema.sql. Lib tests
-- record_and_query + limit_is_respected failed because of this.
CREATE TABLE IF NOT EXISTS crash_log (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          INTEGER NOT NULL,
  kind        TEXT NOT NULL,
  message     TEXT NOT NULL,
  stack       TEXT,
  build_hash  TEXT,
  handled     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_crash_log_ts ON crash_log(ts);

-- Same class of bug as crash_log (wave 172) / issues.message (wave 175):
-- worktrees.rs has always queried this table and its module comment claims
-- the schema lives here, but the CREATE was never added — every worktree
-- insert/list failed at runtime ("no such table: worktrees").
CREATE TABLE IF NOT EXISTS worktrees (
  id           TEXT PRIMARY KEY,
  project_root TEXT NOT NULL,
  branch       TEXT NOT NULL,
  path         TEXT NOT NULL,
  session_id   TEXT,
  created_at   INTEGER NOT NULL,
  archived_at  INTEGER,
  status       TEXT NOT NULL DEFAULT 'active',
  notes        TEXT
);
CREATE INDEX IF NOT EXISTS idx_worktrees_project ON worktrees(project_root, status);

-- Multi-provider lanes (P0-FINAL "Lanes: stop fire-and-forget"). One row per
-- dispatched gateway lane run so the run list survives tab switches and app
-- restarts. `status`: running | done | error | stopped | interrupted —
-- terminal states are never overwritten (see lanes.rs).
CREATE TABLE IF NOT EXISTS lane_runs (
  run_id     TEXT PRIMARY KEY,
  provider   TEXT NOT NULL,
  owner      TEXT NOT NULL,
  repo       TEXT NOT NULL,
  task       TEXT NOT NULL,
  branch     TEXT,
  status     TEXT NOT NULL,
  detail     TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  -- Set when the lane branch was merged into the project's default branch
  -- from the in-app review ("merge winner"). NULL = not merged from Cortex.
  merged_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_lane_runs_created ON lane_runs(created_at);

