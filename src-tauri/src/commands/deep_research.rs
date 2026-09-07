//! In-app Deep Research — multi-step web search → page fetch → cited synthesis,
//! saved into the Brain.
//!
//! Flow: an LLM planner turns the question into a handful of focused search
//! queries; each is run through a keyless DuckDuckGo HTML search; the top unique
//! sources are fetched (reusing `context::fetch_url`, which carries the SSRF
//! guard + HTML→markdown); an LLM analyst synthesizes a report that cites the
//! numbered sources inline; the result is written as a markdown note under the
//! vault's `research/` dir so it's re-openable. Progress streams over
//! `deep_research:progress` events.
//!
//! The parsing/formatting helpers are pure functions, unit-tested without a
//! live network or LLM.

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
pub struct ResearchProgress {
    pub step: String,
    pub status: String,
    pub message: Option<String>,
    pub pct: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchSource {
    pub n: usize,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchReport {
    pub question: String,
    pub markdown: String,
    pub sources: Vec<ResearchSource>,
    pub saved_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedReport {
    pub title: String,
    pub path: String,
    pub question: String,
    pub created_unix_ms: i64,
}

/// The research run currently in flight (question + latest progress), if any.
/// Mirrors `cookbook::ACTIVE_PULLS`: the run itself outlives the `invoke()`
/// that started it, so after a webview reload the frontend job store queries
/// `deep_research_active` to re-adopt it instead of orphaning the work.
/// `Option` rather than a map: progress streams over ONE shared
/// `deep_research:progress` event with no per-run key, so concurrent runs
/// would corrupt each other's UI — `deep_research` rejects a second run.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveResearch {
    pub question: String,
    pub progress: ResearchProgress,
}

static ACTIVE_RESEARCH: Lazy<Mutex<Option<ActiveResearch>>> = Lazy::new(|| Mutex::new(None));

// DDG search engine (decode/parse/fetch) lives in `crate::websearch` — the
// single source of truth shared with the chat `@websearch:` provider. The
// search-result type is the only symbol used outside that module's own fns.
use crate::websearch::WebResult;

// ----- pure helpers (unit-tested) -----

/// Parse the planner's output into a list of search queries: a JSON array if it
/// emitted one, else non-empty lines stripped of bullets/numbering/quotes.
fn parse_search_queries(out: &str) -> Vec<String> {
    if let (Some(a), Some(b)) = (out.find('['), out.rfind(']')) {
        if b > a {
            if let Ok(v) = serde_json::from_str::<Vec<String>>(&out[a..=b]) {
                let v: Vec<String> = v.into_iter().map(|s| s.trim().to_string()).filter(|s| s.len() > 2).collect();
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }
    out.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
                .trim()
                .trim_matches('"')
                .trim()
                .to_string()
        })
        .filter(|l| l.len() > 3)
        .take(6)
        .collect()
}

fn slugify(s: &str) -> String {
    let lowered: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let collapsed = lowered
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    collapsed.chars().take(60).collect::<String>().trim_matches('-').to_string()
}

fn yaml_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn extract_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        if let Some(v) = l.trim().strip_prefix(&prefix) {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn first_heading(content: &str) -> Option<String> {
    content.lines().find_map(|l| l.trim().strip_prefix("# ").map(|s| s.trim().to_string()))
}

// ----- network / disk -----

async fn ddg_search(query: &str) -> Result<Vec<WebResult>, String> {
    // Research fans out many queries and reads many pages, so cast a wide net
    // per query (dedup happens across queries in the caller).
    crate::websearch::search(query, 50).await
}

/// One-shot LLM completion via the Cortex Gateway: send system+user, collect the
/// streamed deltas into the full text.
async fn llm_complete(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let client = GatewayClient::new(base_url.to_string(), api_key.to_string());
    let req = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system.into() },
            ChatMessage { role: "user".into(), content: user.into() },
        ],
        stream: true,
        temperature: Some(0.3),
    };
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
    let (_, body) = tokio::join!(stream_fut, collect_fut);
    if body.trim().is_empty() {
        Err("the model returned an empty response".into())
    } else {
        Ok(body)
    }
}

fn research_dir(vault: &Option<PathBuf>) -> Option<PathBuf> {
    let root = vault
        .clone()
        .or_else(|| dirs::home_dir().map(|h| h.join("Documents").join("Cortex Brain")))?;
    Some(root.join("research"))
}

fn save_report(vault: &Option<PathBuf>, question: &str, markdown: &str) -> Result<PathBuf, String> {
    let dir = research_dir(vault).ok_or("could not resolve a research directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create research dir: {e}"))?;
    let date = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let path = dir.join(format!("{date}-{}.md", slugify(question)));
    let frontmatter = format!(
        "---\nquestion: {}\ncreated: {}\nkind: research\n---\n\n",
        yaml_escape(question),
        chrono::Utc::now().to_rfc3339(),
    );
    std::fs::write(&path, format!("{frontmatter}{markdown}")).map_err(|e| format!("write report: {e}"))?;
    Ok(path)
}

// ----- Tauri commands -----

/// Snapshot of the research run currently in flight, if any. The frontend job
/// store queries this on boot so a webview reload mid-run re-adopts the
/// running job instead of orphaning it.
#[tauri::command]
pub fn deep_research_active() -> Result<Option<ActiveResearch>, String> {
    Ok(ACTIVE_RESEARCH.lock().clone())
}

#[tauri::command]
pub async fn deep_research(
    question: String,
    max_sources: Option<usize>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ResearchReport, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Enter a research question.".into());
    }
    {
        let mut active = ACTIVE_RESEARCH.lock();
        if let Some(run) = active.as_ref() {
            return Err(format!("A research run is already in progress ({}).", run.question));
        }
        *active = Some(ActiveResearch {
            question: question.clone(),
            progress: ResearchProgress {
                step: "starting".into(),
                status: "start".into(),
                message: None,
                pct: 0,
            },
        });
    }
    let result = run_research(question, max_sources, app, state).await;
    *ACTIVE_RESEARCH.lock() = None;
    result
}

async fn run_research(
    question: String,
    max_sources: Option<usize>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ResearchReport, String> {
    let max_sources = max_sources.unwrap_or(5).clamp(1, 10);
    let (base_url, model, vault) = {
        let cfg = state.config.read();
        (cfg.gateway_base_url.clone(), cfg.gateway_model.clone(), cfg.obsidian_vault.clone())
    };
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();

    let emit = |step: &str, status: &str, message: Option<String>, pct: u32| {
        let progress = ResearchProgress { step: step.into(), status: status.into(), message, pct };
        // Keep the in-flight registry current so a reload re-adopts the run at
        // its real progress, not "starting".
        if let Some(run) = ACTIVE_RESEARCH.lock().as_mut() {
            run.progress = progress.clone();
        }
        let _ = app.emit("deep_research:progress", progress);
    };

    // 1. plan the queries
    emit("planning", "start", None, 5);
    let plan = llm_complete(
        &base_url,
        &api_key,
        &model,
        "You are a research planner. Given a question, output 3-5 focused web-search queries that together cover it. Output ONLY a JSON array of strings — no prose.",
        &question,
    )
    .await
    .unwrap_or_default();
    let mut queries = parse_search_queries(&plan);
    if queries.is_empty() {
        queries = vec![question.clone()];
    }
    emit("planning", "done", Some(format!("{} queries", queries.len())), 15);

    // 2. search the web
    emit("searching", "start", None, 20);
    let mut hits: Vec<WebResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for q in &queries {
        if let Ok(found) = ddg_search(q).await {
            for h in found {
                if seen.insert(h.url.clone()) {
                    hits.push(h);
                }
            }
        }
    }
    if hits.is_empty() {
        return Err("No search results came back — is outbound web access available on this machine?".into());
    }
    hits.truncate(max_sources);
    emit("searching", "done", Some(format!("{} sources", hits.len())), 35);

    // 3. read the sources
    emit("reading", "start", None, 40);
    let total = hits.len().max(1) as u32;
    let mut sources: Vec<ResearchSource> = Vec::new();
    let mut corpus = String::new();
    for (i, h) in hits.iter().enumerate() {
        emit("reading", "progress", Some(h.url.clone()), 40 + (i as u32 * 30 / total));
        let text = match crate::commands::context::fetch_url(h.url.clone()).await {
            Ok(page) => page.markdown,
            Err(_) => String::new(),
        };
        // Skip sources that failed to fetch or came back empty, so the report
        // never cites a blank [n] / dead link. Numbering follows kept sources.
        if text.trim().is_empty() {
            continue;
        }
        let n = sources.len() + 1;
        let title = if h.title.trim().is_empty() { h.url.clone() } else { h.title.clone() };
        sources.push(ResearchSource { n, title: title.clone(), url: h.url.clone() });
        let snippet: String = text.chars().take(4000).collect();
        corpus.push_str(&format!("\n\n[{n}] {title} ({})\n{snippet}\n", h.url));
    }
    if sources.is_empty() {
        return Err("Fetched no readable sources for this question — try rephrasing it.".into());
    }
    emit("reading", "done", None, 70);

    // 4. synthesize a cited report
    emit("synthesizing", "start", None, 75);
    let body = llm_complete(
        &base_url,
        &api_key,
        &model,
        "You are a research analyst. Write a thorough, well-structured markdown report answering the user's question using ONLY the numbered sources provided. Cite claims inline with [n] matching the source numbers. Use markdown headings. Do not invent sources or facts. Do not append a reference list — it is added automatically.",
        &format!("Question: {question}\n\nSources:\n{corpus}"),
    )
    .await?;
    emit("synthesizing", "done", None, 90);

    let mut markdown = format!("# {question}\n\n{}\n\n## Sources\n", body.trim());
    for s in &sources {
        markdown.push_str(&format!("{}. [{}]({})\n", s.n, s.title, s.url));
    }

    // 5. save into the Brain
    emit("saving", "start", None, 95);
    let saved_path = save_report(&vault, &question, &markdown).ok().map(|p| p.display().to_string());
    emit("saving", "done", saved_path.clone(), 100);

    Ok(ResearchReport { question, markdown, sources, saved_path })
}

#[tauri::command]
pub fn list_research_reports(state: State<'_, AppState>) -> Result<Vec<SavedReport>, String> {
    let vault = state.config.read().obsidian_vault.clone();
    let Some(dir) = research_dir(&vault) else { return Ok(vec![]) };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let question = extract_frontmatter_field(&content, "question")
                .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
            let title = first_heading(&content).unwrap_or_else(|| question.clone());
            let created_unix_ms = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(SavedReport { title, path: path.display().to_string(), question, created_unix_ms });
        }
    }
    out.sort_by(|a, b| b.created_unix_ms.cmp(&a.created_unix_ms));
    Ok(out)
}

#[tauri::command]
pub fn read_research_report(path: String, state: State<'_, AppState>) -> Result<String, String> {
    // Confine reads to the research directory (defense-in-depth: the path comes
    // from the frontend). Canonicalize both sides and require containment.
    let vault = state.config.read().obsidian_vault.clone();
    let dir = research_dir(&vault).ok_or("could not resolve the research directory")?;
    let canon_dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let canon = std::fs::canonicalize(&path).map_err(|e| format!("report not found: {e}"))?;
    if !canon.starts_with(&canon_dir) {
        return Err("refusing to read a path outside the research directory".into());
    }
    std::fs::read_to_string(&canon).map_err(|e| format!("could not read report: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_from_json_array() {
        let out = "Here you go:\n[\"rust async runtime\", \"tokio vs async-std\"]";
        assert_eq!(parse_search_queries(out), vec!["rust async runtime", "tokio vs async-std"]);
    }

    #[test]
    fn queries_fallback_to_lines() {
        let out = "1. first query here\n- second query line\n\"third one\"";
        let q = parse_search_queries(out);
        assert_eq!(q, vec!["first query here", "second query line", "third one"]);
    }

    // DDG decode/percent-decode/parse are unit-tested in `crate::websearch`
    // (their new home); deep_research keeps only its own pure-helper tests.

    #[test]
    fn slugify_is_filesystem_safe() {
        assert_eq!(slugify("What is the best IPTV setup?? (2026)"), "what-is-the-best-iptv-setup-2026");
    }

    #[test]
    fn frontmatter_and_heading_roundtrip() {
        let doc = "---\nquestion: \"how do tides work\"\ncreated: 2026-06-06\n---\n\n# How do tides work\n\nbody";
        assert_eq!(extract_frontmatter_field(doc, "question").as_deref(), Some("how do tides work"));
        assert_eq!(first_heading(doc).as_deref(), Some("How do tides work"));
    }
}
