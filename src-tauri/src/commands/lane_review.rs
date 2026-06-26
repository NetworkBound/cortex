//! In-app review & merge of lane branches (P0-FINAL "Lanes", slice 2/2).
//!
//! A finished lane is a branch (`cortex/<run>/<provider>`) sitting on Gitea —
//! slice 1 ended with copy telling the user to go merge it by hand. This
//! module closes that loop server-side: "Review" ensures a pull request from
//! the lane branch into the project's default branch exists (Gitea's PR is
//! the natural artifact for a candidate change — the diff shown is exactly
//! what the merge will apply, and conflict state comes free as `mergeable`),
//! fetches the PR's combined `.diff`, and "Merge winner" merges that PR via
//! the API, stamping `merged_at` on the lane row.
//!
//! Gitea access follows the no-hardcoded-endpoints rule (`infra_config`):
//! env override → Gitea backup settings (`~/.cortex/gitea-config.json`) →
//! `update_gitea_host` from `~/.cortex/infra.json` for the URL. Nothing
//! configured → a humanized "not configured" error and NO network I/O.

use crate::commands::gitea_backup;
use crate::lanes::{LaneRunRecord, LaneStore};
use crate::observability::tracing_store::TracingStore;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

/// Mirrors `multi_provider::LANES_UPDATED` (the pane listens on one event).
const LANES_UPDATED: &str = "lanes:updated";

/// Reviews of huge lane branches stay usable: the diff text handed to the UI
/// is capped here, with an honest truncation note appended.
const DIFF_CAP_BYTES: usize = 400_000;

fn not_configured() -> String {
    "Gitea isn't configured — set the base URL and token in Settings → Gitea backup \
     (or CORTEX_GITEA_URL / CORTEX_GITEA_TOKEN) to review and merge lane branches."
        .to_string()
}

/// Resolved Gitea endpoint + credential for lane review. Public so the live
/// integration test can drive the real client against a scratch repo.
#[derive(Clone)]
pub struct GiteaAccess {
    pub base_url: String,
    pub token: String,
}

/// env → backup settings → infra host (URL only). `Err` is the humanized
/// not-configured message; callers must perform no network I/O in that case.
pub fn resolve_gitea_access() -> Result<GiteaAccess, String> {
    let settings = gitea_backup::load_settings();
    let base_url = std::env::var("CORTEX_GITEA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(settings.base_url.clone()).filter(|s| !s.trim().is_empty()))
        .or_else(crate::infra_config::update_gitea_host)
        .ok_or_else(not_configured)?;
    let token = std::env::var("CORTEX_GITEA_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITEA_TOKEN").ok())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(settings.token.clone()).filter(|s| !s.trim().is_empty()))
        .ok_or_else(not_configured)?;
    Ok(GiteaAccess { base_url: base_url.trim_end_matches('/').to_string(), token })
}

/// The slice of a Gitea pull request the review surface needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: i64,
    pub html_url: String,
    /// `open` | `closed` (a merged PR is `closed` with `merged: true`).
    pub state: String,
    pub merged: bool,
    /// Gitea's conflict check: `false` means merging needs manual resolution.
    pub mergeable: bool,
    pub title: String,
    pub base: String,
    pub head: String,
}

#[derive(Debug, Deserialize)]
struct RawBranchRef {
    #[serde(rename = "ref")]
    r#ref: String,
}

#[derive(Debug, Deserialize)]
struct RawPr {
    number: i64,
    html_url: String,
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    mergeable: bool,
    title: String,
    base: RawBranchRef,
    head: RawBranchRef,
}

impl From<RawPr> for PrInfo {
    fn from(p: RawPr) -> Self {
        PrInfo {
            number: p.number,
            html_url: p.html_url,
            state: p.state,
            merged: p.merged,
            mergeable: p.mergeable,
            title: p.title,
            base: p.base.r#ref,
            head: p.head.r#ref,
        }
    }
}

/// Minimal Gitea pulls client (create / find / diff / merge). Public for the
/// live integration test.
pub struct GiteaPrClient {
    access: GiteaAccess,
    http: reqwest::Client,
}

impl GiteaPrClient {
    pub fn new(access: GiteaAccess) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self { access, http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.access.base_url, path)
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("Authorization", format!("token {}", self.access.token))
    }

    /// `GET /repos/{owner}/{repo}` → the branch lane PRs merge into.
    pub async fn default_branch(&self, owner: &str, repo: &str) -> Result<String, String> {
        let resp = self
            .auth(self.http.get(self.url(&format!("repos/{owner}/{repo}"))))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "Gitea doesn't know the repo {owner}/{repo} — check the lane's project."
            ));
        }
        if !resp.status().is_success() {
            return Err(format!("Gitea repo lookup failed ({})", resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        v.get("default_branch")
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Gitea repo answer had no default branch".into())
    }

    /// Newest PR (any state) whose head is `head_branch`, if one exists.
    pub async fn find_pr_by_head(
        &self,
        owner: &str,
        repo: &str,
        head_branch: &str,
    ) -> Result<Option<PrInfo>, String> {
        let resp = self
            .auth(self.http.get(self.url(&format!(
                "repos/{owner}/{repo}/pulls?state=all&limit=50&sort=recentupdate"
            ))))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Gitea PR list failed ({})", resp.status()));
        }
        let prs: Vec<RawPr> = resp.json().await.map_err(|e| e.to_string())?;
        let mut found: Vec<PrInfo> =
            prs.into_iter().map(PrInfo::from).filter(|p| p.head == head_branch).collect();
        // Prefer the open PR; otherwise the most recently updated (first).
        found.sort_by_key(|p| p.state != "open");
        Ok(found.into_iter().next())
    }

    /// Create the lane PR, or adopt the existing one on 409 ("a pull request
    /// for these targets already exists"). A 409 with NO matching PR means
    /// Gitea refused because there is nothing to merge.
    pub async fn ensure_pr(
        &self,
        owner: &str,
        repo: &str,
        head_branch: &str,
        base_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo, String> {
        let resp = self
            .auth(self.http.post(self.url(&format!("repos/{owner}/{repo}/pulls"))))
            .json(&serde_json::json!({
                "base": base_branch,
                "head": head_branch,
                "title": title,
                "body": body,
            }))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        match resp.status() {
            s if s.is_success() => {
                let pr: RawPr = resp.json().await.map_err(|e| e.to_string())?;
                Ok(pr.into())
            }
            reqwest::StatusCode::CONFLICT => {
                if let Some(pr) = self.find_pr_by_head(owner, repo, head_branch).await? {
                    return Ok(pr);
                }
                Err(format!(
                    "Nothing to review — {head_branch} has no commits beyond {base_branch}. \
                     The lane may not have pushed any work."
                ))
            }
            reqwest::StatusCode::NOT_FOUND => Err(format!(
                "Gitea couldn't find the lane branch {head_branch} on {owner}/{repo} — \
                 the lane may have failed before pushing."
            )),
            s => {
                let text = resp.text().await.unwrap_or_default();
                Err(format!("Gitea refused to open the review PR ({s}): {text}"))
            }
        }
    }

    pub async fn pr(&self, owner: &str, repo: &str, index: i64) -> Result<PrInfo, String> {
        let resp = self
            .auth(self.http.get(self.url(&format!("repos/{owner}/{repo}/pulls/{index}"))))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Gitea PR lookup failed ({})", resp.status()));
        }
        let pr: RawPr = resp.json().await.map_err(|e| e.to_string())?;
        Ok(pr.into())
    }

    /// `GET /repos/{owner}/{repo}/pulls/{index}.diff` — the combined unified
    /// diff of the whole PR (exactly what merging applies).
    pub async fn pr_diff(&self, owner: &str, repo: &str, index: i64) -> Result<String, String> {
        let resp = self
            .auth(self.http.get(self.url(&format!("repos/{owner}/{repo}/pulls/{index}.diff"))))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Gitea diff fetch failed ({})", resp.status()));
        }
        resp.text().await.map_err(|e| e.to_string())
    }

    /// `POST /repos/{owner}/{repo}/pulls/{index}/merge` with a plain merge
    /// commit. Gitea answers 405 when the PR has conflicts or is closed.
    pub async fn merge_pr(&self, owner: &str, repo: &str, index: i64) -> Result<(), String> {
        let resp = self
            .auth(self.http.post(self.url(&format!("repos/{owner}/{repo}/pulls/{index}/merge"))))
            .json(&serde_json::json!({ "Do": "merge" }))
            .send()
            .await
            .map_err(|e| format!("couldn't reach Gitea: {e}"))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::METHOD_NOT_ALLOWED => Err(
                "Gitea can't merge this branch automatically — it has conflicts with the \
                 base branch (or was already closed). Resolve them on Gitea, then retry."
                    .into(),
            ),
            s => {
                let text = resp.text().await.unwrap_or_default();
                Err(format!("Gitea merge failed ({s}): {text}"))
            }
        }
    }
}

/// Everything the review panel renders for one lane.
#[derive(Debug, Serialize)]
pub struct LaneReview {
    pub run_id: String,
    pub branch: String,
    pub base: String,
    pub pr_number: i64,
    pub pr_url: String,
    pub state: String,
    pub merged: bool,
    pub mergeable: bool,
    pub title: String,
    /// Combined unified diff (capped at [`DIFF_CAP_BYTES`] with a note).
    pub diff: String,
    pub diff_truncated: bool,
}

fn lane_store(app: &tauri::AppHandle) -> LaneStore {
    LaneStore::new(app.state::<TracingStore>().inner().shared_connection())
}

/// A lane eligible for review: it exists, it pushed a branch, and it isn't
/// still moving.
fn reviewable_lane(store: &LaneStore, run_id: &str) -> Result<(LaneRunRecord, String), String> {
    let lane = store
        .get(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("lane '{run_id}' not found"))?;
    if lane.status == "running" {
        return Err("This lane is still running — wait for it to settle before reviewing.".into());
    }
    let branch = lane
        .branch
        .clone()
        .ok_or_else(|| "This lane never produced a branch — there's nothing to review.".to_string())?;
    Ok((lane, branch))
}

/// Compose the PR title for a lane: provider + a trimmed slice of the task.
pub fn lane_pr_title(provider: &str, task: &str) -> String {
    let task = task.trim().replace('\n', " ");
    let head: String = task.chars().take(72).collect();
    let ellipsis = if task.chars().count() > 72 { "…" } else { "" };
    format!("[cortex lane] {provider}: {head}{ellipsis}")
}

/// Open (or adopt) the review PR for a settled lane and return its combined
/// diff + merge state. Review is user-initiated, so creating the PR here is
/// the point — it is the server-side artifact the merge happens through.
#[tauri::command]
pub async fn lane_review(run_id: String, app: tauri::AppHandle) -> Result<LaneReview, String> {
    let store = lane_store(&app);
    let (lane, branch) = reviewable_lane(&store, &run_id)?;
    let client = GiteaPrClient::new(resolve_gitea_access()?);
    let base = client.default_branch(&lane.owner, &lane.repo).await?;
    let pr = client
        .ensure_pr(
            &lane.owner,
            &lane.repo,
            &branch,
            &base,
            &lane_pr_title(&lane.provider, &lane.task),
            &format!(
                "Lane run `{run_id}` ({provider}) from Cortex — review & merge of the \
                 parallel-provider branch.\n\nTask:\n\n> {task}",
                provider = lane.provider,
                task = lane.task.trim()
            ),
        )
        .await?;
    let mut diff = client.pr_diff(&lane.owner, &lane.repo, pr.number).await?;
    let mut truncated = false;
    if diff.len() > DIFF_CAP_BYTES {
        let mut cut = DIFF_CAP_BYTES;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n… diff truncated — open the PR on Gitea for the full change.\n");
        truncated = true;
    }
    Ok(LaneReview {
        run_id,
        branch,
        base: pr.base.clone(),
        pr_number: pr.number,
        pr_url: pr.html_url.clone(),
        state: pr.state.clone(),
        merged: pr.merged,
        mergeable: pr.mergeable,
        title: pr.title.clone(),
        diff,
        diff_truncated: truncated,
    })
}

/// Merge the lane's review PR ("merge winner") and stamp the row `merged_at`.
/// The PR is re-checked head-against-branch so a stale panel can never merge
/// someone else's PR into the project.
#[tauri::command]
pub async fn merge_lane_run(
    run_id: String,
    pr_number: i64,
    app: tauri::AppHandle,
) -> Result<LaneRunRecord, String> {
    let store = lane_store(&app);
    let (lane, branch) = reviewable_lane(&store, &run_id)?;
    let client = GiteaPrClient::new(resolve_gitea_access()?);
    let pr = client.pr(&lane.owner, &lane.repo, pr_number).await?;
    if pr.head != branch {
        return Err(format!(
            "PR #{pr_number} is for branch {} — not this lane's branch {branch}. \
             Re-open the review and try again.",
            pr.head
        ));
    }
    if !pr.merged {
        client.merge_pr(&lane.owner, &lane.repo, pr_number).await?;
    }
    store
        .mark_merged(&run_id, &format!("Merged into {} (PR #{})", pr.base, pr.number))
        .map_err(|e| e.to_string())?;
    let _ = app.emit(LANES_UPDATED, &run_id);
    store
        .get(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("lane '{run_id}' vanished after merge"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_title_trims_and_caps() {
        assert_eq!(
            lane_pr_title("claude", "fix the login bug\nand tests"),
            "[cortex lane] claude: fix the login bug and tests"
        );
        let long = "x".repeat(200);
        let t = lane_pr_title("gpt", &long);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() < 110);
    }

    #[test]
    fn access_resolution_env_and_not_configured() {
        // Serialize env mutation: this test owns these vars.
        std::env::set_var("CORTEX_GITEA_URL", "http://git.example:3000/");
        std::env::set_var("CORTEX_GITEA_TOKEN", "tok123");
        let a = resolve_gitea_access().expect("env-configured access");
        assert_eq!(a.base_url, "http://git.example:3000"); // trailing slash trimmed
        assert_eq!(a.token, "tok123");
        std::env::remove_var("CORTEX_GITEA_URL");
        std::env::remove_var("CORTEX_GITEA_TOKEN");
    }

    /// Live end-to-end review/merge against a REAL Gitea — the exact client
    /// the `lane_review`/`merge_lane_run` commands drive. Ignored by default;
    /// run with:
    ///   CORTEX_GITEA_URL=http://<gitea> CORTEX_GITEA_TOKEN=<pat> \
    ///   cargo test --lib commands::lane_review::tests::lane_review_live_gitea -- --ignored --nocapture
    /// Creates a private scratch repo, commits a change on a lane-style branch
    /// via the contents API, then proves the full chain: default_branch →
    /// ensure_pr (create) → ensure_pr again (409-adopt, idempotent) → pr_diff
    /// (contains the change) → merge_pr → pr (merged). The scratch repo is
    /// deleted afterwards, pass or fail.
    #[tokio::test]
    #[ignore]
    async fn lane_review_live_gitea() {
        use base64::Engine as _;

        let base = std::env::var("CORTEX_GITEA_URL").expect("set CORTEX_GITEA_URL");
        let token = std::env::var("CORTEX_GITEA_TOKEN").expect("set CORTEX_GITEA_TOKEN");
        let access =
            GiteaAccess { base_url: base.trim_end_matches('/').to_string(), token: token.clone() };
        let client = GiteaPrClient::new(access.clone());
        let http = reqwest::Client::new();
        let auth = |rb: reqwest::RequestBuilder| rb.header("Authorization", format!("token {token}"));

        let me: serde_json::Value = auth(http.get(format!("{}/api/v1/user", access.base_url)))
            .send()
            .await
            .expect("reach Gitea")
            .json()
            .await
            .expect("user json");
        let owner = me["login"].as_str().expect("login").to_string();

        let repo = format!("cortex-lane-review-live-{}", uuid::Uuid::new_v4().simple());
        let resp = auth(http.post(format!("{}/api/v1/user/repos", access.base_url)))
            .json(&serde_json::json!({ "name": repo, "auto_init": true, "private": true }))
            .send()
            .await
            .expect("create repo");
        assert!(resp.status().is_success(), "create scratch repo: {}", resp.status());

        // Everything after repo creation runs inside a closure so the scratch
        // repo is deleted on every exit path before any assert can bail out.
        let run = || async {
            let lane_branch = "cortex/live-test/claude".to_string();
            let base_branch = client.default_branch(&owner, &repo).await?;

            let resp = auth(http.post(format!(
                "{}/api/v1/repos/{owner}/{repo}/contents/lane-note.md",
                access.base_url
            )))
            .json(&serde_json::json!({
                "content": base64::engine::general_purpose::STANDARD.encode("reviewed from cortex\n"),
                "message": "lane work (live test)",
                "branch": base_branch,
                "new_branch": lane_branch,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("contents API: {}", resp.status()));
            }

            let pr = client
                .ensure_pr(&owner, &repo, &lane_branch, &base_branch, "[cortex lane] live test", "body")
                .await?;
            if pr.head != lane_branch || pr.state != "open" {
                return Err(format!("created PR has wrong shape: {pr:?}"));
            }
            // Idempotent: a second ensure adopts the same PR via the 409 path.
            let again = client
                .ensure_pr(&owner, &repo, &lane_branch, &base_branch, "[cortex lane] live test", "body")
                .await?;
            if again.number != pr.number {
                return Err(format!("ensure_pr not idempotent: {} vs {}", again.number, pr.number));
            }

            let diff = client.pr_diff(&owner, &repo, pr.number).await?;
            if !diff.contains("lane-note.md") || !diff.contains("reviewed from cortex") {
                return Err(format!("diff missing the lane change:\n{diff}"));
            }

            client.merge_pr(&owner, &repo, pr.number).await?;
            let merged = client.pr(&owner, &repo, pr.number).await?;
            if !merged.merged {
                return Err(format!("PR not reported merged after merge: {merged:?}"));
            }
            Ok::<i64, String>(pr.number)
        };
        let outcome = run().await;

        let del = auth(http.delete(format!("{}/api/v1/repos/{owner}/{repo}", access.base_url)))
            .send()
            .await;
        let deleted = del.map(|r| r.status().is_success()).unwrap_or(false);

        let pr_number = outcome.expect("live review/merge chain");
        assert!(deleted, "scratch repo {owner}/{repo} was NOT deleted — remove it by hand");
        println!("live gitea review/merge OK — PR #{pr_number} on {owner}/{repo} (repo deleted)");
    }

    #[test]
    fn raw_pr_parses_gitea_shape() {
        let raw: RawPr = serde_json::from_str(
            r#"{
                "number": 7,
                "html_url": "http://git/o/r/pulls/7",
                "state": "open",
                "merged": false,
                "mergeable": true,
                "title": "[cortex lane] claude: do it",
                "base": {"ref": "master", "label": "master"},
                "head": {"ref": "cortex/run1/claude", "label": "cortex/run1/claude"}
            }"#,
        )
        .expect("parse");
        let pr: PrInfo = raw.into();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.base, "master");
        assert_eq!(pr.head, "cortex/run1/claude");
        assert!(pr.mergeable);
    }
}
