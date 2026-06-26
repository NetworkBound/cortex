//! Batch runner — CrewAI-style `kickoff_for_each`.
//!
//! Runs one prompt template across N items in parallel via the gateway. The
//! frontend collects live progress through `batch:progress:<run_id>` window
//! events (status pill flips, partial output stream) and the final
//! `BatchRunReport` is the returned promise.
//!
//! Items are arbitrary strings. When an item happens to resolve to a file on
//! disk, the first 16 KiB of its content is prepended to the substituted
//! prompt as context — that's the "pick from project files" affordance from
//! the modal. The substitution token is the literal `{{item}}`; templates
//! without it still work (each call shares the same prompt body).
//!
//! Parallelism caps at 8 to keep the gateway from melting; the default is 4.
//! Each item gets its own `tokio::spawn` slot guarded by a `Semaphore`, so a
//! batch of 50 items with parallelism=4 keeps four streams hot at any moment.

use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tauri::{Emitter, State};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// 16 KiB cap when an item resolves to a file path. Smaller than the
/// per-call doc_gen/test_gen ceilings because we may run dozens of these in
/// parallel — total context still needs to fit.
const FILE_CONTEXT_LIMIT: usize = 16 * 1024;

/// Hard ceiling on per-item wall clock. Streaming runs that get stuck on a
/// hung connection bubble up as `error` items instead of hanging the batch.
const PER_ITEM_TIMEOUT_S: u64 = 120;

/// Default parallelism when the caller doesn't specify one. CrewAI uses 5; 4
/// matches the gateway's default request budget more comfortably.
const DEFAULT_PARALLELISM: usize = 4;

/// Absolute ceiling on parallel requests. Above this the gateway starts queueing
/// upstream and the UI loses its "live" feel.
const MAX_PARALLELISM: usize = 8;

/// Hard ceiling on batch size — 200 items is plenty for a single kickoff and
/// prevents pathological payloads from a slipped-paste.
const MAX_ITEMS: usize = 200;

#[allow(dead_code)] // variants are constructed via serde + frontend, not by name in Rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Queued,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchItem {
    pub index: usize,
    pub item: String,
    pub status: BatchStatus,
    pub output: String,
    pub tokens: u64,
    pub latency_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchRunReport {
    pub run_id: String,
    pub started_unix_ms: i64,
    pub completed_unix_ms: i64,
    pub items: Vec<BatchItem>,
}

#[derive(Debug, Clone, Serialize)]
struct ProgressPayload<'a> {
    item_index: usize,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[tauri::command]
pub async fn batch_run(
    items: Vec<String>,
    prompt_template: String,
    parallelism: Option<usize>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<BatchRunReport, String> {
    if items.is_empty() {
        return Err("batch: items is empty".into());
    }
    if items.len() > MAX_ITEMS {
        return Err(format!(
            "batch: too many items ({}, max {MAX_ITEMS})",
            items.len()
        ));
    }
    if prompt_template.trim().is_empty() {
        return Err("batch: prompt_template is empty".into());
    }

    let pll = parallelism
        .unwrap_or(DEFAULT_PARALLELISM)
        .clamp(1, MAX_PARALLELISM);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = Arc::new(GatewayClient::new(cfg.gateway_base_url, api_key));
    let model = cfg.gateway_model.clone();

    let run_id = ulid::Ulid::new().to_string();
    let event_name = format!("batch:progress:{run_id}");
    let started_unix_ms = chrono::Utc::now().timestamp_millis();

    let semaphore = Arc::new(Semaphore::new(pll));
    let template = Arc::new(prompt_template);
    let mut set: JoinSet<BatchItem> = JoinSet::new();

    for (idx, raw_item) in items.into_iter().enumerate() {
        let sem = semaphore.clone();
        let client = client.clone();
        let template = template.clone();
        let model = model.clone();
        let app = app.clone();
        let event_name = event_name.clone();

        // Emit "queued" up front so the table renders before the worker spins.
        emit_progress(&app, &event_name, idx, "queued", None, None);

        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            run_item(idx, raw_item, &template, &model, &client, &app, &event_name).await
        });
    }

    let mut items_out: Vec<BatchItem> = Vec::with_capacity(set.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(item) => items_out.push(item),
            Err(e) => {
                items_out.push(BatchItem {
                    index: items_out.len(),
                    item: String::new(),
                    status: BatchStatus::Error,
                    output: String::new(),
                    tokens: 0,
                    latency_ms: 0,
                    error: Some(format!("join error: {e}")),
                });
            }
        }
    }
    items_out.sort_by_key(|i| i.index);

    let completed_unix_ms = chrono::Utc::now().timestamp_millis();
    Ok(BatchRunReport {
        run_id,
        started_unix_ms,
        completed_unix_ms,
        items: items_out,
    })
}

async fn run_item(
    index: usize,
    raw_item: String,
    template: &str,
    model: &str,
    client: &GatewayClient,
    app: &tauri::AppHandle,
    event_name: &str,
) -> BatchItem {
    let started = Instant::now();
    emit_progress(app, event_name, index, "running", None, None);

    let prompt = build_prompt(template, &raw_item);

    let req = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: prompt,
        }],
        stream: true,
        temperature: Some(0.2),
    };

    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
    let client_clone = client.clone();
    let stream_handle = tokio::spawn(async move {
        let _ = client_clone.chat_completion_stream(req, tx).await;
    });

    let app_for_stream = app.clone();
    let event_for_stream = event_name.to_string();
    let collect_fut = async move {
        let mut buf = String::new();
        let mut tokens: u64 = 0;
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => {
                    buf.push_str(&s);
                    emit_progress(
                        &app_for_stream,
                        &event_for_stream,
                        index,
                        "running",
                        Some(buf.clone()),
                        None,
                    );
                }
                StreamItem::Done { usage } => {
                    if let Some(u) = usage {
                        tokens = u.total_tokens;
                    }
                    break;
                }
            }
        }
        (buf, tokens)
    };

    let timeout = std::time::Duration::from_secs(PER_ITEM_TIMEOUT_S);
    let result = tokio::time::timeout(timeout, collect_fut).await;
    // Make sure the SSE task is reaped — it may still be inflight on timeout.
    stream_handle.abort();

    match result {
        Ok((output, tokens)) => {
            let trimmed = output.trim().to_string();
            if trimmed.is_empty() {
                let err = "empty response from the gateway".to_string();
                emit_progress(app, event_name, index, "error", None, Some(err.clone()));
                BatchItem {
                    index,
                    item: raw_item,
                    status: BatchStatus::Error,
                    output: String::new(),
                    tokens,
                    latency_ms: started.elapsed().as_millis(),
                    error: Some(err),
                }
            } else {
                emit_progress(
                    app,
                    event_name,
                    index,
                    "done",
                    Some(trimmed.clone()),
                    None,
                );
                BatchItem {
                    index,
                    item: raw_item,
                    status: BatchStatus::Done,
                    output: trimmed,
                    tokens,
                    latency_ms: started.elapsed().as_millis(),
                    error: None,
                }
            }
        }
        Err(_) => {
            let err = format!("timed out after {PER_ITEM_TIMEOUT_S}s");
            emit_progress(app, event_name, index, "error", None, Some(err.clone()));
            BatchItem {
                index,
                item: raw_item,
                status: BatchStatus::Error,
                output: String::new(),
                tokens: 0,
                latency_ms: started.elapsed().as_millis(),
                error: Some(err),
            }
        }
    }
}

fn emit_progress(
    app: &tauri::AppHandle,
    event_name: &str,
    item_index: usize,
    status: &str,
    partial_output: Option<String>,
    error: Option<String>,
) {
    let payload = ProgressPayload {
        item_index,
        status,
        partial_output,
        error,
    };
    let _ = app.emit(event_name, payload);
}

/// Build the per-item prompt. `{{item}}` is substituted; when the raw item
/// resolves to an existing file, the first FILE_CONTEXT_LIMIT bytes of its
/// content are prepended as a fenced context block so the model sees both
/// the path AND the body. Non-file items just get the templated string.
fn build_prompt(template: &str, raw_item: &str) -> String {
    let substituted = template.replace("{{item}}", raw_item);
    if let Some(body) = read_file_context(raw_item) {
        format!(
            "--- CONTEXT (file `{raw_item}`) ---\n{body}\n--- END CONTEXT ---\n\n{substituted}"
        )
    } else {
        substituted
    }
}

/// Returns the truncated file body when `raw_item` is a readable file path.
/// Best-effort — silently returns `None` for URLs, ticket IDs, missing
/// files, or unreadable paths.
fn read_file_context(raw_item: &str) -> Option<String> {
    let p = Path::new(raw_item);
    if !p.is_file() {
        return None;
    }
    let raw = std::fs::read_to_string(p).ok()?;
    Some(truncate_at(raw, FILE_CONTEXT_LIMIT))
}

fn truncate_at(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated — file exceeded 16 KiB]");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn build_prompt_substitutes_item_token() {
        let out = build_prompt("Summarise {{item}} in one line.", "alpha-ticket");
        assert_eq!(out, "Summarise alpha-ticket in one line.");
    }

    #[test]
    fn build_prompt_without_token_is_passthrough() {
        let out = build_prompt("Tell me a joke.", "anything");
        assert_eq!(out, "Tell me a joke.");
    }

    #[test]
    fn build_prompt_prepends_file_context_when_path_exists() {
        let mut tmp = tempfile::NamedTempFile::new().expect("tmpfile");
        writeln!(tmp, "hello world").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let out = build_prompt("Refactor {{item}}", &path);
        assert!(out.contains("--- CONTEXT (file"));
        assert!(out.contains("hello world"));
        assert!(out.contains(&format!("Refactor {path}")));
    }

    #[test]
    fn build_prompt_skips_context_for_non_paths() {
        let out = build_prompt("Process {{item}}", "https://example.com/x");
        assert!(!out.contains("--- CONTEXT"));
        assert!(out.contains("https://example.com/x"));
    }

    #[test]
    fn truncate_at_caps_long_blobs() {
        let blob = "x".repeat(FILE_CONTEXT_LIMIT + 200);
        let out = truncate_at(blob, FILE_CONTEXT_LIMIT);
        assert!(out.contains("[truncated"));
        assert!(out.len() < FILE_CONTEXT_LIMIT + 100);
    }

    #[test]
    fn truncate_at_passes_short_blobs_through() {
        let out = truncate_at("short".into(), FILE_CONTEXT_LIMIT);
        assert_eq!(out, "short");
    }
}
