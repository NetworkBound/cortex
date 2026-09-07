//! Issue-to-Agent pipeline (issue 007, MVP).
//!
//! Bridges tracked forge issues (GitHub / GitLab) to the EXISTING lane
//! machinery, with a HARD approval gate before any write leaves the app:
//!
//!   1. `issues_import` — READ-ONLY import of a repo's open issues into a
//!      triage list. Forge tokens come from the encrypted KeyVault ONLY
//!      (provider `"github"` / `"gitlab"`; any label); public repos work
//!      unauthenticated. Tokens are sent as request headers and are never
//!      logged, audited, or returned across the bridge.
//!   2. `issue_run_in_lane` — dispatch an imported issue as a task on the
//!      existing worktree-isolated lane machinery
//!      (`multi_provider::dispatch_team_lane` → gateway `/v1/runs` with a
//!      `cortex_worktree`). The agent run rides the existing model routing
//!      (gateway / Model Fabric); this module adds NO new execution path.
//!   3. `issue_pr_preview` / `issue_open_pr` — the approval gate. Preview is
//!      a pure dry-run (NO network I/O): it renders the draft PR from the
//!      lane + issue and mints a ONE-SHOT, expiring approval token. Only
//!      `issue_open_pr` with a live token performs writes — the review PR on
//!      Gitea (via the same client `lane_review` uses) and a progress
//!      comment on the source issue. No valid token → no write, ever; there
//!      is no auto-push anywhere in this module.
//!
//! Every step lands in the audit log (`issues.*` actions), and audit details
//! plus the outbound comment pass through the `crate::redact` choke point.
//! Safe Mode's state (issue 004) is recorded at dispatch time so the audit
//! trail shows which policy the run started under; the run itself executes
//! server-side under the gateway's own approval machinery, untouched here.

use crate::agents::adapter::AgentEvent;
use crate::commands::lane_review::{resolve_gitea_access, reviewable_lane, GiteaPrClient};
use crate::lanes::{LaneRunRecord, LaneStore};
use crate::observability::tracing_store::TracingStore;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::Manager;

/// One-shot PR approvals expire after this long — a stale "Approve" click
/// hours later must not push anything.
const APPROVAL_TTL_MS: i64 = 15 * 60 * 1000;

/// Issue bodies embedded into a lane task are capped so a pathological issue
/// can't blow up the prompt.
const ISSUE_BODY_CAP_CHARS: usize = 4_000;

// ─────────────────────────────── forge model ───────────────────────────────

/// Which issue tracker we're talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    GitHub,
    GitLab,
}

impl ForgeKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "github" => Ok(Self::GitHub),
            "gitlab" => Ok(Self::GitLab),
            other => Err(format!(
                "unknown forge '{other}' — use \"github\" or \"gitlab\""
            )),
        }
    }

    fn default_base(&self) -> &'static str {
        match self {
            Self::GitHub => "https://api.github.com",
            Self::GitLab => "https://gitlab.com",
        }
    }

    /// KeyVault provider name the forge PAT is stored under.
    pub fn vault_provider(&self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        }
    }
}

/// One imported issue, normalized across forges — what the triage list renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeIssue {
    pub forge: String,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub url: String,
    pub labels: Vec<String>,
    pub author: String,
    pub updated_at: String,
    /// Lightweight local heuristic (labels first, keyword fallback) — a
    /// triage hint, not a judgment call an agent or model made. See
    /// [`classify_issue`].
    pub kind: IssueKind,
}

/// Auto-classification (007 full scope): a cheap, local, deterministic guess
/// at what kind of work an issue is, shown as a triage-list hint. Purely a
/// heuristic over labels/title/body text — no model call, no network, no new
/// execution path (classification never blocks or gates the pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueKind {
    Bug,
    Feature,
    Chore,
    Unknown,
}

/// Label text that confidently maps to a kind — checked before the fuzzier
/// keyword fallback since an explicit label is the strongest signal a triager
/// (human or forge convention) already gave us.
fn classify_by_labels(labels: &[String]) -> Option<IssueKind> {
    let lower: Vec<String> = labels.iter().map(|l| l.to_ascii_lowercase()).collect();
    let has = |set: &[&str]| lower.iter().any(|l| set.contains(&l.as_str()));
    if has(&["bug", "defect", "regression", "crash"]) {
        Some(IssueKind::Bug)
    } else if has(&["feature", "enhancement", "feature-request", "feature request"]) {
        Some(IssueKind::Feature)
    } else if has(&[
        "chore", "maintenance", "refactor", "docs", "documentation", "dependencies", "build", "ci",
    ]) {
        Some(IssueKind::Chore)
    } else {
        None
    }
}

/// Keyword fallback over title + body when labels don't say. Deliberately
/// coarse — this is a triage hint, not a verdict, and a false guess costs the
/// user nothing more than re-sorting a card by eye.
fn classify_by_text(title: &str, body: &str) -> IssueKind {
    let text = format!("{} {}", title.to_ascii_lowercase(), body.to_ascii_lowercase());
    let hits = |words: &[&str]| words.iter().any(|w| text.contains(w));
    let is_bug = hits(&[
        "bug", "crash", "error", "fails", "failing", "broken", "doesn't work", "does not work",
        "exception", "panic", "regression", "reproduce",
    ]);
    let is_feature = hits(&[
        "feature", "add support", "implement", "would be nice", "feature request", "enhancement",
        "proposal", "please add", "wish",
    ]);
    let is_chore = hits(&[
        "chore", "refactor", "cleanup", "clean up", "upgrade", "bump", "docs", "documentation",
        "typo", "rename", "dependency",
    ]);
    // Bug reports read the most urgently and are the likeliest false-negative
    // to bury in triage, so they win ties; chore is the narrowest/most benign
    // category and loses every tie.
    match (is_bug, is_feature, is_chore) {
        (true, _, _) => IssueKind::Bug,
        (false, true, _) => IssueKind::Feature,
        (false, false, true) => IssueKind::Chore,
        _ => IssueKind::Unknown,
    }
}

/// Classify one issue for the triage list: labels first, keyword heuristic
/// otherwise. Pure and total — every issue gets a kind, `Unknown` included.
pub fn classify_issue(labels: &[String], title: &str, body: &str) -> IssueKind {
    classify_by_labels(labels).unwrap_or_else(|| classify_by_text(title, body))
}

/// The slice of an issue later stages need (lane dispatch, PR draft, the
/// progress comment). `base_url` carries a self-hosted forge base along.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRef {
    pub forge: String,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Owner/repo segments are plain names — anything else could rewrite the
/// request path (traversal, query injection) before it reaches the forge.
fn is_safe_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        // "." / ".." are path segments, not repo names.
        && !s.chars().all(|c| c == '.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn bad_slug() -> String {
    "owner and repo must be plain names (letters, digits, '.', '_', '-')".to_string()
}

/// Base URL for a forge: the caller's self-hosted override or the public host.
fn forge_base(forge: ForgeKind, base_url: Option<&str>) -> String {
    base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| forge.default_base().to_string())
}

/// READ endpoint listing a repo's open issues.
pub fn issues_list_url(
    forge: ForgeKind,
    base_url: Option<&str>,
    owner: &str,
    repo: &str,
) -> String {
    let base = forge_base(forge, base_url);
    match forge {
        ForgeKind::GitHub => {
            format!("{base}/repos/{owner}/{repo}/issues?state=open&per_page=50")
        }
        // GitLab addresses projects by URL-encoded `owner/repo`; owner and
        // repo are slug-validated, so `%2F` is the only escape needed.
        ForgeKind::GitLab => format!(
            "{base}/api/v4/projects/{owner}%2F{repo}/issues?state=opened&per_page=50"
        ),
    }
}

/// WRITE endpoint for posting a progress comment on an issue.
pub fn issue_comment_url(
    forge: ForgeKind,
    base_url: Option<&str>,
    owner: &str,
    repo: &str,
    number: i64,
) -> String {
    let base = forge_base(forge, base_url);
    match forge {
        ForgeKind::GitHub => format!("{base}/repos/{owner}/{repo}/issues/{number}/comments"),
        ForgeKind::GitLab => {
            format!("{base}/api/v4/projects/{owner}%2F{repo}/issues/{number}/notes")
        }
    }
}

// ───────────────────────────── response parsing ────────────────────────────

#[derive(Debug, Deserialize)]
struct RawGithubLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawGithubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RawGithubIssue {
    number: i64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    html_url: String,
    #[serde(default)]
    labels: Vec<RawGithubLabel>,
    #[serde(default)]
    user: Option<RawGithubUser>,
    #[serde(default)]
    updated_at: String,
    /// Present on pull requests — GitHub's issues endpoint returns PRs too,
    /// and a PR is not triageable work for this pipeline.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawGitlabAuthor {
    username: String,
}

#[derive(Debug, Deserialize)]
struct RawGitlabIssue {
    iid: i64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    state: String,
    web_url: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    author: Option<RawGitlabAuthor>,
    #[serde(default)]
    updated_at: String,
}

/// Parse a forge issues response into the normalized triage shape. Pure, so
/// the recorded-fixture tests exercise exactly what production parses.
pub fn parse_issues(
    forge: ForgeKind,
    owner: &str,
    repo: &str,
    body: &str,
) -> Result<Vec<ForgeIssue>, String> {
    match forge {
        ForgeKind::GitHub => {
            let raw: Vec<RawGithubIssue> = serde_json::from_str(body)
                .map_err(|e| format!("couldn't parse the GitHub issues response: {e}"))?;
            Ok(raw
                .into_iter()
                .filter(|i| i.pull_request.is_none())
                .map(|i| {
                    let body = i.body.unwrap_or_default();
                    let labels: Vec<String> = i.labels.into_iter().map(|l| l.name).collect();
                    let kind = classify_issue(&labels, &i.title, &body);
                    ForgeIssue {
                        forge: "github".into(),
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                        number: i.number,
                        title: i.title,
                        body,
                        state: i.state,
                        url: i.html_url,
                        labels,
                        author: i.user.map(|u| u.login).unwrap_or_default(),
                        updated_at: i.updated_at,
                        kind,
                    }
                })
                .collect())
        }
        ForgeKind::GitLab => {
            let raw: Vec<RawGitlabIssue> = serde_json::from_str(body)
                .map_err(|e| format!("couldn't parse the GitLab issues response: {e}"))?;
            Ok(raw
                .into_iter()
                .map(|i| {
                    let body = i.description.unwrap_or_default();
                    let kind = classify_issue(&i.labels, &i.title, &body);
                    ForgeIssue {
                        forge: "gitlab".into(),
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                        number: i.iid,
                        title: i.title,
                        body,
                        state: i.state,
                        url: i.web_url,
                        labels: i.labels,
                        author: i.author.map(|a| a.username).unwrap_or_default(),
                        updated_at: i.updated_at,
                        kind,
                    }
                })
                .collect())
        }
    }
}

// ────────────────────────────── forge transport ────────────────────────────

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// Attach forge auth. The token rides a header only — it can never appear in
/// a URL (and thus never in an error string, which only ever echoes the URL
/// host or a status code).
fn forge_auth(
    rb: reqwest::RequestBuilder,
    forge: ForgeKind,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let rb = rb.header("User-Agent", "cortex-app");
    match (forge, token) {
        (ForgeKind::GitHub, Some(t)) => rb
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {t}")),
        (ForgeKind::GitHub, None) => rb.header("Accept", "application/vnd.github+json"),
        (ForgeKind::GitLab, Some(t)) => rb.header("PRIVATE-TOKEN", t.to_string()),
        (ForgeKind::GitLab, None) => rb,
    }
}

fn forge_status_err(forge: ForgeKind, status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => format!(
            "the forge rejected the request ({status}) — check the \"{}\" key in the Key Vault \
             (or remove it to browse public repos anonymously)",
            forge.vault_provider()
        ),
        404 => "the forge doesn't know that repo — check the owner/repo (private repos need a \
                Key Vault token)"
            .to_string(),
        _ => format!("the forge request failed ({status})"),
    }
}

/// Resolve the forge PAT from the KeyVault ONLY. `Ok(None)` (no key stored)
/// means anonymous access — fine for public repos, and honest errors
/// otherwise. No env vars, no plaintext config, by design.
fn vault_token(forge: ForgeKind) -> Result<Option<String>, String> {
    crate::commands::keyvault::lookup_provider_key_sync(forge.vault_provider())
}

// ─────────────────────────────── audit helper ──────────────────────────────

/// What `audit` sends through the redact choke point. Split out as a pure
/// function so it's unit-testable without a live `AppHandle` — the whole
/// point of the choke point is that a token embedded in ANY audit field (an
/// issue title/body is attacker-controlled text; a caller could also just
/// slip up) never reaches the tracing store.
fn redact_audit_detail(detail: &serde_json::Value) -> String {
    crate::redact::redact_text(&detail.to_string())
}

/// Write an `issues.*` audit row. Details pass through the redact choke
/// point as defense in depth — no caller ever puts a token in a detail, but
/// issue titles/bodies are attacker-controlled text.
fn audit(app: &tauri::AppHandle, action: &str, detail: serde_json::Value) {
    let store = app.state::<TracingStore>();
    let detail = redact_audit_detail(&detail);
    if let Err(e) = store.record_audit(None, None, action, Some(&detail)) {
        tracing::warn!("issues: audit write failed: {e}");
    }
}

fn lane_store(app: &tauri::AppHandle) -> LaneStore {
    LaneStore::new(app.state::<TracingStore>().inner().shared_connection())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Session id for an issue lane's local Run Replay recording — namespaced so
/// it can never collide with (or get folded into the cost/budget accounting
/// of, see issue 006) a real chat session.
fn replay_session_id(run_id: &str) -> String {
    format!("issue-lane-{run_id}")
}

/// Start a minimal local Run Replay recording for an issue-dispatched lane
/// (007 full scope): the lane's `run_id` doubles as both `trace_id` and
/// `agent.run` span id, so `run_replay(run_id)` finds it directly with no
/// extra bridging table. This is local sqlite bookkeeping ONLY — reusing the
/// exact helpers `chat.rs` already uses for every other run — so it adds NO
/// new execution path and NO network egress. Best-effort: a recording
/// failure must never block dispatch, which already succeeded by the time
/// this runs.
fn start_replay_recording(app: &tauri::AppHandle, record: &LaneRunRecord, issue: &IssueRef) {
    let store = app.state::<TracingStore>();
    let trace_id = record.run_id.clone();
    let session_id = replay_session_id(&record.run_id);
    let task = build_lane_task(issue);
    let _ = store.record_chat_turn(
        &trace_id,
        &session_id,
        &task,
        std::slice::from_ref(&record.provider),
        Some("issue-to-agent pipeline (007): dispatched from an imported issue"),
    );
    let _ = store.start_agent_run(
        &record.run_id,
        &trace_id,
        &session_id,
        "issue-lane",
        Some(&record.provider),
    );
}

/// Close out the local Run Replay recording once a human previews the lane's
/// PR — the first guaranteed-reached checkpoint after the lane settles.
/// Best-effort and safe to call more than once (re-previewing just re-stamps
/// the same run; it never fails the preview itself).
fn finish_replay_recording(app: &tauri::AppHandle, run_id: &str) {
    let store = app.state::<TracingStore>();
    let _ = store.record_event(
        run_id,
        &AgentEvent::Done { total_tokens: None, run_id: Some(run_id.to_string()) },
    );
    let _ = store.finish_agent_run(run_id);
}

// ───────────────────────────── 1. read-only import ─────────────────────────

#[derive(Debug, Deserialize)]
pub struct IssueImportArgs {
    /// `"github"` | `"gitlab"`.
    pub forge: String,
    pub owner: String,
    pub repo: String,
    /// Self-hosted forge base (e.g. `https://gitlab.example.com`); public
    /// hosts when omitted.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Import a repo's open issues into the triage list. STRICTLY read-only —
/// one GET, no side effects on the forge. Audited (count only; bodies and
/// tokens stay out of the log).
#[tauri::command]
pub async fn issues_import(
    args: IssueImportArgs,
    app: tauri::AppHandle,
) -> Result<Vec<ForgeIssue>, String> {
    let forge = ForgeKind::parse(&args.forge)?;
    if !is_safe_slug(&args.owner) || !is_safe_slug(&args.repo) {
        return Err(bad_slug());
    }
    let token = vault_token(forge)?;
    let url = issues_list_url(forge, args.base_url.as_deref(), &args.owner, &args.repo);
    let resp = forge_auth(http_client()?.get(&url), forge, token.as_deref())
        .send()
        .await
        .map_err(|e| format!("couldn't reach the forge: {e}"))?;
    if !resp.status().is_success() {
        return Err(forge_status_err(forge, resp.status()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let issues = parse_issues(forge, &args.owner, &args.repo, &body)?;
    audit(
        &app,
        "issues.imported",
        serde_json::json!({
            "forge": forge.name(),
            "repo": format!("{}/{}", args.owner, args.repo),
            "count": issues.len(),
            "authenticated": token.is_some(),
        }),
    );
    Ok(issues)
}

// ──────────────────────────── 2. run in a lane ─────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IssueLaneArgs {
    /// Gitea project the lane works on (the gateway clones `<owner>/<repo>`
    /// into an isolated worktree — same as every other lane).
    pub gitea_owner: String,
    pub gitea_repo: String,
    /// Provider/model id from `list_gateway_models`.
    pub provider: String,
    pub issue: IssueRef,
}

/// The task prompt a lane receives for an issue: title + source link + a
/// capped body. Pure for testability.
pub fn build_lane_task(issue: &IssueRef) -> String {
    let mut body = issue.body.trim().to_string();
    if body.chars().count() > ISSUE_BODY_CAP_CHARS {
        body = body.chars().take(ISSUE_BODY_CAP_CHARS).collect();
        body.push_str("\n… (issue body truncated)");
    }
    let mut out = format!(
        "Resolve issue #{number}: {title}\n\nSource: {url}\n",
        number = issue.number,
        title = issue.title.trim(),
        url = issue.url.trim(),
    );
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }
    out
}

/// Standing instructions pinning the run to its isolated worktree. The lane
/// runs server-side in its own git worktree already; this makes the contract
/// explicit to the agent: NO pushes, NO PRs, NO issue-tracker writes — those
/// happen from Cortex, behind the human approval gate.
pub fn lane_instructions(issue: &IssueRef) -> String {
    format!(
        "You are resolving issue #{number} of {owner}/{repo}. Work entirely inside your \
         assigned git worktree and commit there. Do NOT push to any remote, open pull \
         requests, or write to the issue tracker — Cortex opens the review PR after a \
         human approves it.",
        number = issue.number,
        owner = issue.owner,
        repo = issue.repo,
    )
}

/// Dispatch an issue onto ONE worktree-isolated lane, reusing the exact path
/// the team orchestrator uses (`dispatch_team_lane`). Returns the persisted
/// lane row; progress arrives via `lanes:updated` like every other lane.
#[tauri::command]
pub async fn issue_run_in_lane(
    args: IssueLaneArgs,
    app: tauri::AppHandle,
) -> Result<LaneRunRecord, String> {
    if args.provider.trim().is_empty() {
        return Err("pick a provider to run the issue with".into());
    }
    if !is_safe_slug(&args.gitea_owner) || !is_safe_slug(&args.gitea_repo) {
        return Err(bad_slug());
    }
    let input = build_lane_task(&args.issue);
    let instructions = lane_instructions(&args.issue);
    // Record which policy regime the run started under (Safe Mode, issue
    // 004) — the run executes on the gateway under its own approval
    // machinery; the audit trail must still show the state at dispatch.
    let safe_mode = crate::commands::safe_mode::is_enabled();
    let record = crate::commands::multi_provider::dispatch_team_lane(
        &app,
        &args.gitea_owner,
        &args.gitea_repo,
        &args.provider,
        &input,
        Some(&instructions),
    )
    .await?;
    audit(
        &app,
        "issues.lane-dispatched",
        serde_json::json!({
            "run_id": record.run_id,
            "provider": record.provider,
            "project": format!("{}/{}", record.owner, record.repo),
            "issue": args.issue.url,
            "safe_mode": safe_mode,
        }),
    );
    // Best-effort local Run Replay recording (007 full scope) so the PR this
    // lane may eventually open can point at something real. Never blocks or
    // fails dispatch, which has already succeeded above.
    start_replay_recording(&app, &record, &args.issue);
    Ok(record)
}

// ─────────────────────── 3. approval-gated PR + comment ────────────────────

/// A previewed-but-not-yet-approved PR. Lives only in memory: an app restart
/// drops every pending approval, which is the safe direction.
#[derive(Debug, Clone)]
pub(crate) struct PendingApproval {
    pub run_id: String,
    pub issue: IssueRef,
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub created_ms: i64,
}

static PENDING: Lazy<Mutex<HashMap<String, PendingApproval>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Mint a one-shot approval token for a previewed draft.
pub(crate) fn grant_approval(pending: PendingApproval) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    PENDING.lock().insert(token.clone(), pending);
    token
}

/// Consume an approval token. ONE-SHOT: the entry is removed before any
/// check, so a token can never authorize two writes — and an expired token
/// is both refused and gone.
pub(crate) fn take_approval(token: &str, now_ms: i64) -> Result<PendingApproval, String> {
    let pending = PENDING
        .lock()
        .remove(token)
        .ok_or_else(|| {
            "that approval isn't valid anymore — preview the PR again to approve it".to_string()
        })?;
    if now_ms.saturating_sub(pending.created_ms) > APPROVAL_TTL_MS {
        return Err(
            "that approval expired — preview the PR again to approve it".to_string(),
        );
    }
    Ok(pending)
}

/// Compose the draft PR (title, body) for a lane resolving an issue. Pure —
/// this is the "dry-run PR creation" surface: no client, no network.
pub fn build_pr_draft(lane: &LaneRunRecord, branch: &str, issue: &IssueRef) -> (String, String) {
    let title_head: String = issue.title.trim().replace('\n', " ").chars().take(72).collect();
    let ellipsis = if issue.title.trim().chars().count() > 72 { "…" } else { "" };
    let title = format!(
        "[cortex issue] {}/{}#{}: {title_head}{ellipsis}",
        issue.owner, issue.repo, issue.number
    );
    let body = format!(
        "Draft PR from Cortex's issue pipeline.\n\n\
         Resolves issue: {url}\n\
         Lane run `{run_id}` ({provider}) on branch `{branch}`.\n\n\
         Run Replay: this run's full narration/tool-call timeline is recorded \
         locally in Cortex — open Observability → Run Replay and select run \
         `{run_id}` to see exactly what the agent did before this PR was opened.\n\n\
         Opened after explicit human approval in Cortex — review before merging.",
        url = issue.url.trim(),
        run_id = lane.run_id,
        provider = lane.provider,
    );
    (title, body)
}

/// The progress comment posted back on the source issue, already passed
/// through the redact choke point (it leaves the app).
pub fn progress_comment_outbound(issue: &IssueRef, pr_url: &str, lane: &LaneRunRecord) -> String {
    let raw = format!(
        "Cortex opened a review PR for this issue: {pr_url}\n\n\
         Agent lane `{run_id}` ({provider}) worked issue #{number} (\"{title}\") in an \
         isolated git worktree; a human approved opening the PR.",
        run_id = lane.run_id,
        provider = lane.provider,
        number = issue.number,
        title = issue.title.trim(),
    );
    crate::redact::redact_text(&raw)
}

/// What the approval dialog renders — the full draft plus the one-shot token
/// that authorizes (only) this draft.
#[derive(Debug, Clone, Serialize)]
pub struct IssuePrDraft {
    pub approval_token: String,
    pub run_id: String,
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub expires_unix_ms: i64,
}

/// Dry-run the PR for a settled issue lane. NO network I/O and NO writes —
/// it renders the draft and mints the approval token the user must spend on
/// `issue_open_pr`. Audited.
#[tauri::command]
pub async fn issue_pr_preview(
    run_id: String,
    issue: IssueRef,
    app: tauri::AppHandle,
) -> Result<IssuePrDraft, String> {
    let store = lane_store(&app);
    let (lane, branch) = reviewable_lane(&store, &run_id)?;
    let (title, body) = build_pr_draft(&lane, &branch, &issue);
    // The lane has settled (reviewable_lane above guarantees it) — this is
    // the first guaranteed-reached checkpoint, so close out its local Run
    // Replay recording here. Best-effort; never blocks the preview.
    finish_replay_recording(&app, &run_id);
    let now = now_ms();
    let token = grant_approval(PendingApproval {
        run_id: run_id.clone(),
        issue: issue.clone(),
        title: title.clone(),
        body: body.clone(),
        head_branch: branch.clone(),
        created_ms: now,
    });
    // The token itself stays out of the audit log — it is a (short-lived)
    // write credential.
    audit(
        &app,
        "issues.pr-previewed",
        serde_json::json!({ "run_id": run_id, "issue": issue.url, "head": branch }),
    );
    Ok(IssuePrDraft {
        approval_token: token,
        run_id,
        title,
        body,
        head_branch: branch,
        expires_unix_ms: now + APPROVAL_TTL_MS,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuePrResult {
    pub pr_number: i64,
    pub pr_url: String,
    pub base: String,
    pub head: String,
    /// The progress comment on the source issue is best-effort: a comment
    /// failure must not un-open the PR, so it's reported instead of raised.
    pub comment_posted: bool,
    pub comment_error: Option<String>,
}

/// THE approval gate. Spends a one-shot token from `issue_pr_preview`; only
/// then does anything get written: the review PR on Gitea (same client the
/// lane review uses) and a progress comment on the source issue. An invalid,
/// expired, or replayed token performs NO network I/O and is audited as a
/// rejection.
#[tauri::command]
pub async fn issue_open_pr(
    approval_token: String,
    app: tauri::AppHandle,
) -> Result<IssuePrResult, String> {
    let pending = match take_approval(&approval_token, now_ms()) {
        Ok(p) => p,
        Err(e) => {
            audit(
                &app,
                "issues.pr-approval-rejected",
                serde_json::json!({ "reason": e }),
            );
            return Err(e);
        }
    };
    // Re-check the lane against the store — it may have been deleted (or its
    // branch changed shape) between preview and approval.
    let store = lane_store(&app);
    let (lane, branch) = reviewable_lane(&store, &pending.run_id)?;
    if branch != pending.head_branch {
        return Err(
            "the lane's branch changed since the preview — preview the PR again".to_string(),
        );
    }
    let client = GiteaPrClient::new(resolve_gitea_access()?);
    let base = client.default_branch(&lane.owner, &lane.repo).await?;
    let pr = client
        .ensure_pr(&lane.owner, &lane.repo, &branch, &base, &pending.title, &pending.body)
        .await?;
    audit(
        &app,
        "issues.pr-opened",
        serde_json::json!({
            "run_id": pending.run_id,
            "issue": pending.issue.url,
            "pr": pr.html_url,
            "base": base,
            "head": branch,
        }),
    );
    let comment = progress_comment_outbound(&pending.issue, &pr.html_url, &lane);
    let (comment_posted, comment_error) =
        match post_issue_comment(&pending.issue, &comment).await {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        };
    audit(
        &app,
        if comment_posted { "issues.comment-posted" } else { "issues.comment-failed" },
        serde_json::json!({
            "issue": pending.issue.url,
            "pr": pr.html_url,
            "error": comment_error,
        }),
    );
    Ok(IssuePrResult {
        pr_number: pr.number,
        pr_url: pr.html_url,
        base,
        head: branch,
        comment_posted,
        comment_error,
    })
}

/// POST the (already-redacted) progress comment on the source issue. Needs a
/// KeyVault token — commenting is a write, so anonymous is never attempted.
async fn post_issue_comment(issue: &IssueRef, body_text: &str) -> Result<(), String> {
    let forge = ForgeKind::parse(&issue.forge)?;
    if !is_safe_slug(&issue.owner) || !is_safe_slug(&issue.repo) {
        return Err(bad_slug());
    }
    if issue.number <= 0 {
        return Err("that issue has no valid number".into());
    }
    let token = vault_token(forge)?.ok_or_else(|| {
        format!(
            "no \"{}\" key in the Key Vault — posting the progress comment needs one",
            forge.vault_provider()
        )
    })?;
    let url = issue_comment_url(
        forge,
        issue.base_url.as_deref(),
        &issue.owner,
        &issue.repo,
        issue.number,
    );
    let resp = forge_auth(http_client()?.post(&url), forge, Some(&token))
        .json(&serde_json::json!({ "body": body_text }))
        .send()
        .await
        .map_err(|e| format!("couldn't reach the forge: {e}"))?;
    if !resp.status().is_success() {
        return Err(forge_status_err(forge, resp.status()));
    }
    Ok(())
}

// ────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded (trimmed) GitHub `GET /repos/{o}/{r}/issues` response: two
    /// real issues (one with a null body) and one pull request, which the
    /// endpoint interleaves and the parser must drop.
    const GITHUB_FIXTURE: &str = r#"[
        {
            "number": 42,
            "title": "Crash on startup",
            "body": "Steps to reproduce:\n1. launch\n2. boom",
            "state": "open",
            "html_url": "https://github.com/octocat/hello/issues/42",
            "labels": [{"name": "bug", "color": "d73a4a"}, {"name": "p1", "color": "ffffff"}],
            "user": {"login": "alice", "id": 1},
            "updated_at": "2026-06-30T12:00:00Z",
            "assignee": null,
            "comments": 3
        },
        {
            "number": 43,
            "title": "Body can be null",
            "body": null,
            "state": "open",
            "html_url": "https://github.com/octocat/hello/issues/43",
            "labels": [],
            "user": {"login": "bob", "id": 2},
            "updated_at": "2026-06-29T09:30:00Z"
        },
        {
            "number": 44,
            "title": "A pull request, not an issue",
            "body": "PRs ride the issues endpoint too",
            "state": "open",
            "html_url": "https://github.com/octocat/hello/pull/44",
            "labels": [],
            "user": {"login": "carol", "id": 3},
            "updated_at": "2026-06-28T08:00:00Z",
            "pull_request": {"url": "https://api.github.com/repos/octocat/hello/pulls/44"}
        }
    ]"#;

    /// Recorded (trimmed) GitLab `GET /projects/{id}/issues` response.
    const GITLAB_FIXTURE: &str = r#"[
        {
            "iid": 7,
            "project_id": 99,
            "title": "Improve docs",
            "description": "The README skips setup.",
            "state": "opened",
            "web_url": "https://gitlab.com/grp/proj/-/issues/7",
            "labels": ["docs"],
            "author": {"username": "carol", "id": 5},
            "updated_at": "2026-06-29T08:00:00Z"
        },
        {
            "iid": 9,
            "project_id": 99,
            "title": "No description",
            "description": null,
            "state": "opened",
            "web_url": "https://gitlab.com/grp/proj/-/issues/9",
            "labels": [],
            "author": null,
            "updated_at": "2026-06-27T10:00:00Z"
        }
    ]"#;

    fn lane(run_id: &str) -> LaneRunRecord {
        LaneRunRecord {
            run_id: run_id.into(),
            provider: "claude".into(),
            owner: "octocat".into(),
            repo: "cortex".into(),
            task: "resolve the issue".into(),
            branch: Some(format!("cortex/{run_id}/claude")),
            status: "done".into(),
            detail: None,
            created_at: 1,
            updated_at: 2,
            merged_at: None,
        }
    }

    fn issue() -> IssueRef {
        IssueRef {
            forge: "github".into(),
            owner: "octocat".into(),
            repo: "hello".into(),
            number: 42,
            title: "Crash on startup".into(),
            body: "Steps to reproduce".into(),
            url: "https://github.com/octocat/hello/issues/42".into(),
            base_url: None,
        }
    }

    // ── mocked-forge import (recorded fixtures) ─────────────────────────────

    #[test]
    fn github_fixture_parses_and_drops_pull_requests() {
        let issues = parse_issues(ForgeKind::GitHub, "octocat", "hello", GITHUB_FIXTURE).unwrap();
        assert_eq!(issues.len(), 2, "the PR row must be filtered out");
        assert_eq!(issues[0].number, 42);
        assert_eq!(issues[0].forge, "github");
        assert_eq!(issues[0].owner, "octocat");
        assert_eq!(issues[0].labels, vec!["bug".to_string(), "p1".to_string()]);
        assert_eq!(issues[0].author, "alice");
        assert_eq!(issues[0].kind, IssueKind::Bug, "the \"bug\" label wins over any text heuristic");
        assert_eq!(issues[1].number, 43);
        assert_eq!(issues[1].body, "", "null body normalizes to empty");
    }

    #[test]
    fn gitlab_fixture_parses() {
        let issues = parse_issues(ForgeKind::GitLab, "grp", "proj", GITLAB_FIXTURE).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].number, 7);
        assert_eq!(issues[0].forge, "gitlab");
        assert_eq!(issues[0].labels, vec!["docs".to_string()]);
        assert_eq!(issues[0].author, "carol");
        assert_eq!(issues[0].kind, IssueKind::Chore, "the \"docs\" label maps to chore");
        assert_eq!(issues[1].author, "", "null author normalizes to empty");
        assert_eq!(issues[1].body, "");
    }

    // ── auto-classification (007 full scope) ───────────────────────────────

    #[test]
    fn classify_prefers_explicit_labels_over_text() {
        // A "feature" label wins even though the title reads like a bug.
        assert_eq!(
            classify_issue(&["feature".to_string()], "crash on startup", ""),
            IssueKind::Feature
        );
        assert_eq!(classify_issue(&["bug".to_string()], "add dark mode", ""), IssueKind::Bug);
        assert_eq!(
            classify_issue(&["documentation".to_string()], "", ""),
            IssueKind::Chore
        );
    }

    #[test]
    fn classify_falls_back_to_title_body_keywords() {
        assert_eq!(
            classify_issue(&[], "App crashes on startup", "Steps to reproduce: ..."),
            IssueKind::Bug
        );
        assert_eq!(
            classify_issue(&[], "Please add dark mode support", "Would be nice to have"),
            IssueKind::Feature
        );
        assert_eq!(
            classify_issue(&[], "Refactor the auth module", "Just a cleanup, no behavior change"),
            IssueKind::Chore
        );
        assert_eq!(
            classify_issue(&[], "Question about setup", "How do I configure X?"),
            IssueKind::Unknown
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_issues(ForgeKind::GitHub, "o", "r", "not json").is_err());
        assert!(parse_issues(ForgeKind::GitLab, "o", "r", "{\"not\": \"a list\"}").is_err());
    }

    #[test]
    fn forge_kind_parses_case_insensitively() {
        assert_eq!(ForgeKind::parse("GitHub").unwrap(), ForgeKind::GitHub);
        assert_eq!(ForgeKind::parse(" gitlab ").unwrap(), ForgeKind::GitLab);
        assert!(ForgeKind::parse("bitbucket").is_err());
    }

    #[test]
    fn list_urls_hit_the_right_endpoints() {
        assert_eq!(
            issues_list_url(ForgeKind::GitHub, None, "octocat", "hello"),
            "https://api.github.com/repos/octocat/hello/issues?state=open&per_page=50"
        );
        // GitLab path-encodes owner/repo; self-hosted bases get their
        // trailing slash trimmed.
        assert_eq!(
            issues_list_url(ForgeKind::GitLab, Some("https://git.example.com/"), "grp", "proj"),
            "https://git.example.com/api/v4/projects/grp%2Fproj/issues?state=opened&per_page=50"
        );
    }

    #[test]
    fn comment_urls_hit_the_right_endpoints() {
        assert_eq!(
            issue_comment_url(ForgeKind::GitHub, None, "octocat", "hello", 42),
            "https://api.github.com/repos/octocat/hello/issues/42/comments"
        );
        assert_eq!(
            issue_comment_url(ForgeKind::GitLab, None, "grp", "proj", 7),
            "https://gitlab.com/api/v4/projects/grp%2Fproj/issues/7/notes"
        );
    }

    #[test]
    fn slugs_block_path_injection() {
        assert!(is_safe_slug("octocat"));
        assert!(is_safe_slug("hello-world.js_2"));
        assert!(!is_safe_slug(""));
        assert!(!is_safe_slug("a/b"));
        assert!(!is_safe_slug(".."));
        assert!(!is_safe_slug("a b"));
        assert!(!is_safe_slug("a?x=1"));
        assert!(!is_safe_slug("a#frag"));
    }

    // ── lane dispatch (worktree isolation contract) ─────────────────────────

    #[test]
    fn lane_task_embeds_issue_and_caps_body() {
        let mut i = issue();
        i.body = "x".repeat(10_000);
        let task = build_lane_task(&i);
        assert!(task.contains("Resolve issue #42: Crash on startup"));
        assert!(task.contains("https://github.com/octocat/hello/issues/42"));
        assert!(task.contains("… (issue body truncated)"));
        assert!(task.chars().count() < ISSUE_BODY_CAP_CHARS + 300);

        // Empty body → no trailing body block, no truncation note.
        let mut empty = issue();
        empty.body = String::new();
        assert!(!build_lane_task(&empty).contains("truncated"));
    }

    /// The isolation contract for issue lanes: the run happens in the lane's
    /// own worktree (the dispatch path always sets a `cortex_worktree`), and
    /// the standing instructions forbid every write the approval gate owns.
    /// Store-level isolation (one row/branch per run) is covered in
    /// `crate::lanes` tests.
    #[test]
    fn lane_instructions_pin_the_worktree_and_forbid_pushes() {
        let text = lane_instructions(&issue());
        assert!(text.contains("inside your assigned git worktree"));
        assert!(text.contains("Do NOT push"));
        assert!(text.contains("open pull requests"), "{text}");
        assert!(text.contains("issue tracker"));
        assert!(text.contains("#42"));
    }

    // ── dry-run PR creation ────────────────────────────────────────────────

    #[test]
    fn pr_draft_dry_run_is_pure_and_complete() {
        let l = lane("run-1");
        let (title, body) = build_pr_draft(&l, "cortex/run-1/claude", &issue());
        assert_eq!(title, "[cortex issue] octocat/hello#42: Crash on startup");
        assert!(body.contains("https://github.com/octocat/hello/issues/42"));
        assert!(body.contains("`run-1`"));
        assert!(body.contains("cortex/run-1/claude"));
        assert!(body.contains("human approval"));

        // Long titles are capped with an ellipsis.
        let mut long = issue();
        long.title = "y".repeat(200);
        let (t2, _) = build_pr_draft(&l, "b", &long);
        assert!(t2.ends_with('…'));
        assert!(t2.chars().count() < 120);
    }

    /// Templated PR bodies (007 full scope) must point at the run's Run
    /// Replay recording (issue 001) by the same id `run_replay(span_id)`
    /// looks up — `build_pr_draft` and `start_replay_recording` both key off
    /// `lane.run_id`, so this is the literal contract between the two.
    #[test]
    fn pr_body_links_the_run_replay_recording() {
        let l = lane("replay-run-1");
        let (_, body) = build_pr_draft(&l, "cortex/replay-run-1/claude", &issue());
        assert!(body.contains("Run Replay"), "{body}");
        assert!(
            body.contains("replay-run-1"),
            "the PR body must reference the exact run id Run Replay is keyed by: {body}"
        );
    }

    /// The outbound comment passes through the redact choke point — a token
    /// smuggled into attacker-controlled issue text must never reach the
    /// forge.
    #[test]
    fn progress_comment_is_redacted() {
        let mut i = issue();
        i.title = "leak ghp_0123456789abcdefghijklmnopqrstuvwxyz here".into();
        let comment = progress_comment_outbound(&i, "http://git/pr/1", &lane("run-1"));
        assert!(!comment.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyz"));
        assert!(comment.contains("[REDACTED]"));
        assert!(comment.contains("http://git/pr/1"));
        assert!(comment.contains("`run-1`"));
    }

    // ── the approval gate ──────────────────────────────────────────────────

    fn pending(run_id: &str, created_ms: i64) -> PendingApproval {
        PendingApproval {
            run_id: run_id.into(),
            issue: issue(),
            title: "t".into(),
            body: "b".into(),
            head_branch: format!("cortex/{run_id}/claude"),
            created_ms,
        }
    }

    #[test]
    fn approval_gate_is_one_shot() {
        let token = grant_approval(pending("gate-run", 1_000));
        // A wrong token never authorizes anything.
        assert!(take_approval("not-a-token", 1_001).is_err());
        // The right token works exactly once…
        let p = take_approval(&token, 1_001).expect("first take succeeds");
        assert_eq!(p.run_id, "gate-run");
        // …and a replay is refused.
        assert!(take_approval(&token, 1_002).is_err());
    }

    #[test]
    fn approval_gate_expires_and_burns_the_token() {
        let token = grant_approval(pending("expiring-run", 1_000));
        let too_late = 1_000 + APPROVAL_TTL_MS + 1;
        assert!(take_approval(&token, too_late).is_err(), "expired token refused");
        // The expired token was consumed on the failed attempt — it can't be
        // retried at an earlier `now` either.
        assert!(take_approval(&token, 1_001).is_err());
    }

    /// An approval is bound to the lane it previewed: the pending record
    /// carries the exact run + head branch, and `issue_open_pr` re-checks the
    /// branch against the store before writing — a token minted for one lane
    /// can never open a PR for another.
    #[test]
    fn approval_is_scoped_to_its_lane() {
        let token_a = grant_approval(pending("lane-a", 1_000));
        let token_b = grant_approval(pending("lane-b", 1_000));
        let a = take_approval(&token_a, 1_001).unwrap();
        let b = take_approval(&token_b, 1_001).unwrap();
        assert_eq!(a.run_id, "lane-a");
        assert_eq!(a.head_branch, "cortex/lane-a/claude");
        assert_eq!(b.run_id, "lane-b");
        assert_ne!(token_a, token_b);
    }

    // ── tokens never reach the audit log ────────────────────────────────────

    /// The audit choke point (`redact_audit_detail`, what every `audit()` call
    /// in this module sends to the tracing store) must scrub a token shape no
    /// matter which field it rides in or which forge it's shaped like — an
    /// issue title/body is attacker-controlled text that ends up in several
    /// audit details (`issues.imported`, `issues.pr-previewed`, …).
    #[test]
    fn audit_detail_never_leaks_a_token() {
        let detail = serde_json::json!({
            "issue": "https://github.com/octocat/hello/issues/42",
            "note": "a GitHub PAT ghp_0123456789abcdefghijklmnopqrstuvwxyz snuck in here",
            "other": "a GitLab PAT glpat-0123456789abcdefghij too",
        });
        let out = redact_audit_detail(&detail);
        assert!(!out.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyz"));
        assert!(!out.contains("glpat-0123456789abcdefghij"));
        assert!(out.contains("[REDACTED]"));
        // Non-secret structure survives — the audit trail stays readable.
        assert!(out.contains("https://github.com/octocat/hello/issues/42"));
    }
}
