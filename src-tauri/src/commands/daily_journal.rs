//! Daily activity journal.
//!
//! `/journal [YYYY-MM-DD]` collates everything that happened today across the
//! Cortex telemetry surfaces — sessions started, commits landed, memory
//! entries touched, snapshots taken, PRP stage advances — and asks the gateway to
//! summarise the day in tight markdown. The structured stats are returned
//! alongside the prose so the UI can render a "scoreboard" header above the
//! narrative.
//!
//! Mirrors the streaming-collect + brain-save pattern from
//! [`super::session_summary`] / [`super::explain`].

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::observability::tracing_store::TracingStore;

const SYSTEM_PROMPT: &str = "You are a daily journal writer. Summarize today's work activity \
in markdown using these section headers: ## Sessions / ## Commits / ## Memory Updates / ## Other. \
Be brief and honest — don't pad. If a section has nothing to report, say so in one line.";

/// 45s wall clock — the journal can pull in dozens of inputs so we give it a
/// little more rope than the session summarizer.
const TIMEOUT: Duration = Duration::from_secs(45);

/// Cap the raw activity blob we feed the model. 24 KiB easily fits even a
/// busy day's commits + sessions + memory.
const MAX_BUNDLE_CHARS: usize = 24_000;

#[derive(Debug, Deserialize)]
pub struct JournalArgs {
    pub project_root: Option<String>,
    /// `YYYY-MM-DD`; defaults to local today when missing or blank.
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalStats {
    pub sessions: usize,
    pub commits: usize,
    pub memory_updates: usize,
    pub snapshots: usize,
    pub prp_advances: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct JournalReport {
    pub date: String,
    pub markdown: String,
    pub stats: JournalStats,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn daily_journal(
    args: JournalArgs,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<JournalReport, String> {
    let date = resolve_date(args.date.as_deref())?;
    let (day_start_ms, day_end_ms) = day_bounds_ms(&date)?;
    let project_root = args.project_root.as_ref().map(PathBuf::from);

    // 1. Sessions started today — tokens_by_session gives us a recent window
    //    with `last_active_ms`. We treat "in the day" as the activity hit.
    let sessions_today: Vec<String> = store
        .tokens_by_session(200)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.last_active_ms >= day_start_ms && s.last_active_ms < day_end_ms)
        .map(|s| format!("- {} ({} runs, {} tokens)", s.session_id, s.runs, s.total_tokens))
        .collect();

    // 2. Commits made today via `git log --since=<date>`.
    let commits_today: Vec<String> = match project_root.as_deref() {
        Some(root) if root.is_dir() => git_commits_for_day(root, &date),
        _ => Vec::new(),
    };

    // 3. Memory entries created/touched today — file mtime under the
    //    aggregated memory sources.
    let memory_updates: Vec<String> = collect_memory_updates(
        project_root.as_deref(),
        state.config.read().obsidian_vault.clone().as_deref(),
        day_start_ms,
        day_end_ms,
    );

    // 4. Snapshots created today.
    let snapshots_today: Vec<String> = crate::memory::snapshots::list()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.created_unix_ms >= day_start_ms && s.created_unix_ms < day_end_ms)
        .map(|s| format!("- {} ({})", s.label, s.id))
        .collect();

    // 5. PRP stage advances — best-effort. We can't tell *when* a stage was
    //    advanced, so we count PRPs whose `created_unix_ms` falls inside the
    //    day as a proxy for activity. Beats nothing.
    let prp_advances: Vec<String> = match project_root.as_deref() {
        Some(root) => crate::prp::list_prps(root)
            .into_iter()
            .filter(|p| p.created_unix_ms >= day_start_ms && p.created_unix_ms < day_end_ms)
            .map(|p| format!("- {} → {}", p.name, p.status.as_str()))
            .collect(),
        None => Vec::new(),
    };

    let stats = JournalStats {
        sessions: sessions_today.len(),
        commits: commits_today.len(),
        memory_updates: memory_updates.len(),
        snapshots: snapshots_today.len(),
        prp_advances: prp_advances.len(),
    };

    let bundle = build_activity_bundle(
        &date,
        &sessions_today,
        &commits_today,
        &memory_updates,
        &snapshots_today,
        &prp_advances,
    );

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: bundle },
        ],
        stream: true,
        temperature: Some(0.3),
    };

    let markdown = run_with_timeout(client, req).await?;
    let cleaned = sanitize(&markdown);
    if cleaned.is_empty() {
        return Err("journal returned empty markdown".into());
    }

    Ok(JournalReport {
        date,
        markdown: cleaned,
        stats,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

async fn run_with_timeout(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);

    let stream_fut = async move {
        let _ = client.chat_completion_stream(req, tx).await;
    };

    let collect_fut = async {
        let mut buf = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => buf.push_str(&s),
                StreamItem::Done { .. } => break,
            }
        }
        buf
    };

    match tokio::time::timeout(TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!(
            "journal timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn resolve_date(raw: Option<&str>) -> Result<String, String> {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(chrono::Local::now().format("%Y-%m-%d").to_string());
    }
    // Validate `YYYY-MM-DD`. We don't need the parsed value past validation —
    // `day_bounds_ms` will re-parse — but we want to reject garbage early.
    chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map_err(|e| format!("invalid date `{trimmed}` (want YYYY-MM-DD): {e}"))?;
    Ok(trimmed.to_string())
}

/// Returns `[day_start, day_end)` in unix-ms, anchored to the *local*
/// timezone so "today" matches the user's wall clock.
fn day_bounds_ms(date: &str) -> Result<(i64, i64), String> {
    use chrono::{NaiveDate, TimeZone};
    let nd = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("bad date {date}: {e}"))?;
    let start_dt = nd
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| "bad date math".to_string())?;
    let end_dt = nd
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .ok_or_else(|| "bad date math".to_string())?;
    let start = chrono::Local
        .from_local_datetime(&start_dt)
        .single()
        .ok_or_else(|| "ambiguous local start".to_string())?
        .timestamp_millis();
    let end = chrono::Local
        .from_local_datetime(&end_dt)
        .single()
        .ok_or_else(|| "ambiguous local end".to_string())?
        .timestamp_millis();
    Ok((start, end))
}

/// `git log --since=YYYY-MM-DD --until=<next-day>` parsed to `- <short> <subject>`.
fn git_commits_for_day(project_root: &Path, date: &str) -> Vec<String> {
    let next_day = match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.succ_opt())
    {
        Some(d) => d.format("%Y-%m-%d").to_string(),
        None => return Vec::new(),
    };
    let output = crate::sys::no_window("git")
        .args([
            "log",
            "--pretty=format:%h|%an|%s",
            "--no-color",
            &format!("--since={date}"),
            &format!("--until={next_day}"),
            "--all",
            "-200",
        ])
        .current_dir(project_root)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let mut parts = l.splitn(3, '|');
                    let hash = parts.next().unwrap_or("");
                    let author = parts.next().unwrap_or("");
                    let subject = parts.next().unwrap_or("");
                    format!("- {hash} {subject} ({author})")
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn collect_memory_updates(
    project_root: Option<&Path>,
    vault: Option<&Path>,
    day_start_ms: i64,
    day_end_ms: i64,
) -> Vec<String> {
    let mut out = Vec::new();
    let srcs = crate::memory::sources::default_sources(project_root, vault);
    for src in &srcs {
        for p in crate::memory::sources::walk_markdown(src) {
            let mtime_ms = std::fs::metadata(&p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if mtime_ms >= day_start_ms && mtime_ms < day_end_ms {
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.display().to_string());
                out.push(format!("- {} [{}]", name, src.label));
            }
        }
    }
    // Stable ordering so the LLM prompt is deterministic across runs.
    out.sort();
    out
}

fn build_activity_bundle(
    date: &str,
    sessions: &[String],
    commits: &[String],
    memory: &[String],
    snapshots: &[String],
    prps: &[String],
) -> String {
    let body = format!(
        "Date: {date}\n\n\
         ## Sessions\n{sessions_block}\n\n\
         ## Commits\n{commits_block}\n\n\
         ## Memory Updates\n{memory_block}\n\n\
         ## Snapshots\n{snapshots_block}\n\n\
         ## PRP Activity\n{prps_block}\n\n\
         Summarise the day in tight markdown using the four required headers \
         (Sessions / Commits / Memory Updates / Other). Roll Snapshots + PRP \
         Activity into the Other section. Don't invent details not present above.",
        sessions_block = empty_or_join(sessions, "(none)"),
        commits_block = empty_or_join(commits, "(none)"),
        memory_block = empty_or_join(memory, "(none)"),
        snapshots_block = empty_or_join(snapshots, "(none)"),
        prps_block = empty_or_join(prps, "(none)"),
    );
    truncate(body, MAX_BUNDLE_CHARS)
}

fn empty_or_join(items: &[String], placeholder: &str) -> String {
    if items.is_empty() {
        placeholder.to_string()
    } else {
        items.join("\n")
    }
}

fn truncate(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[…activity bundle truncated…]");
    s
}

fn sanitize(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim().to_string();
        }
    }
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s
}

// ----- Save to brain -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SaveJournalArgs {
    pub date: String,
    pub markdown: String,
    pub stats: JournalStats,
}

#[derive(Debug, Serialize)]
pub struct SaveJournalResult {
    pub written_path: PathBuf,
    pub bytes: usize,
}

#[tauri::command]
pub async fn save_journal(args: SaveJournalArgs) -> Result<SaveJournalResult, String> {
    // Validate the date in the same way `daily_journal` did so we can't be
    // tricked into writing `../passwd.md`.
    let date = resolve_date(Some(&args.date))?;
    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    let dir = brain_root.join("journal");
    fs::create_dir_all(&dir).map_err(|e| format!("create journal dir failed: {e}"))?;

    let filename = format!("{date}.md");
    let written_path = dir.join(&filename);
    if !written_path.starts_with(&dir) {
        return Err("refusing to write outside the brain vault".into());
    }

    let now_iso = chrono::Utc::now().to_rfc3339();
    let frontmatter = format!(
        "---\nkind: daily-journal\ndate: {date}\ngenerated_at: {now_iso}\n\
         sessions: {s}\ncommits: {c}\nmemory_updates: {m}\nsnapshots: {sn}\nprp_advances: {pa}\n---\n\n",
        s = args.stats.sessions,
        c = args.stats.commits,
        m = args.stats.memory_updates,
        sn = args.stats.snapshots,
        pa = args.stats.prp_advances,
    );
    let body = format!("{frontmatter}# Daily journal — {date}\n\n{}\n", args.markdown.trim());
    let bytes = body.as_bytes().len();
    fs::write(&written_path, &body)
        .map_err(|e| format!("write {} failed: {e}", written_path.display()))?;

    Ok(SaveJournalResult { written_path, bytes })
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_date_defaults_to_today() {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(resolve_date(None).unwrap(), today);
        assert_eq!(resolve_date(Some("")).unwrap(), today);
        assert_eq!(resolve_date(Some("   ")).unwrap(), today);
    }

    #[test]
    fn resolve_date_validates_format() {
        assert_eq!(resolve_date(Some("2026-05-27")).unwrap(), "2026-05-27");
        assert!(resolve_date(Some("not-a-date")).is_err());
        assert!(resolve_date(Some("2026/05/27")).is_err());
    }

    #[test]
    fn day_bounds_orders_start_before_end() {
        let (s, e) = day_bounds_ms("2026-05-27").unwrap();
        assert!(e > s);
        // A day is 24h ± DST.
        let diff = e - s;
        assert!(diff >= 23 * 3_600_000 && diff <= 25 * 3_600_000, "{diff}");
    }

    #[test]
    fn empty_or_join_handles_empty_and_filled() {
        assert_eq!(empty_or_join(&[], "(none)"), "(none)");
        assert_eq!(
            empty_or_join(&["a".to_string(), "b".to_string()], "(none)"),
            "a\nb"
        );
    }

    #[test]
    fn build_activity_bundle_includes_all_sections() {
        let s = build_activity_bundle(
            "2026-05-27",
            &["- s1".into()],
            &["- c1".into()],
            &[],
            &[],
            &[],
        );
        assert!(s.contains("Date: 2026-05-27"));
        assert!(s.contains("## Sessions\n- s1"));
        assert!(s.contains("## Commits\n- c1"));
        assert!(s.contains("## Memory Updates\n(none)"));
    }

    #[test]
    fn truncate_caps_long_blobs() {
        let blob = "x".repeat(MAX_BUNDLE_CHARS + 500);
        let out = truncate(blob, MAX_BUNDLE_CHARS);
        assert!(out.contains("truncated"));
        assert!(out.len() < MAX_BUNDLE_CHARS + 100);
    }

    #[test]
    fn sanitize_strips_fence() {
        let raw = "```markdown\n## Sessions\nstuff\n```";
        assert_eq!(sanitize(raw), "## Sessions\nstuff");
    }

    #[test]
    fn sanitize_strips_preamble() {
        let raw = "Here is your journal:\n## Sessions\nstuff";
        assert_eq!(sanitize(raw), "## Sessions\nstuff");
    }
}
