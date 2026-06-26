//! `rerank` Tauri command — LLM-backed second-stage reranker on top of the
//! heuristic [`crate::retrieval::retrieve_blended`] candidate set.
//!
//! Stage 1 (blocking, deterministic): gather + dedup + heuristic-rerank the
//! blended candidates exactly like the `retrieve` command. Stage 2 (one gateway
//! call): ask the gateway to reorder those candidates by relevance to the query and
//! apply the returned permutation. The gateway answer is a cheap index list, so
//! the model never has to echo paths/snippets back.
//!
//! Best-effort by contract: any gateway error, timeout, or unparseable answer
//! falls back to the heuristic order — `/rerank` can never return worse than
//! `/retrieve`. Mirrors the streaming-collect + timeout pattern in
//! [`super::arch_diagram`].

use std::time::Duration;

use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::retrieval::{
    apply_rank_order, build_rerank_prompt, parse_rank_order, retrieve_blended, RetrievalHit,
};
use tauri::State;

/// Wall-clock budget for the single rerank gateway call. Reranking is a light
/// reordering task, so it gets a tighter budget than diagram synthesis; on
/// timeout we silently keep the heuristic order.
const RERANK_TIMEOUT: Duration = Duration::from_secs(20);

/// How many heuristic candidates to feed the model before it reranks. Larger
/// than the typical returned `k` so the LLM can promote a candidate the
/// heuristic buried.
const CANDIDATE_POOL: usize = 20;

const RERANK_SYSTEM: &str =
    "You are a precise retrieval reranker. Given a developer query and a numbered \
     list of candidate context snippets, output the candidate indices ordered from \
     most to least relevant to the query. Respond with ONLY the indices, \
     comma-separated. Do not add prose or code fences.";

/// Retrieve, LLM-rerank, and return the top-`k` context hits for `query`.
///
/// - `query` must be non-empty (after trimming).
/// - `project_root` must exist and be a directory.
/// - `k` defaults to 6 when omitted.
#[tauri::command]
pub async fn rerank(
    project_root: String,
    query: String,
    k: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<RetrievalHit>, String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".into());
    }
    let root = std::path::PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let k = k.unwrap_or(6);

    // Stage 1 — heuristic candidate pool (blocking fs + sqlite work).
    let q = query.clone();
    let candidates: Vec<RetrievalHit> =
        tokio::task::spawn_blocking(move || retrieve_blended(&root, &q, CANDIDATE_POOL))
            .await
            .map_err(|e| format!("join error: {e}"))?;

    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // Nothing to reorder.
    if candidates.len() == 1 {
        return Ok(candidates);
    }

    // Stage 2 — LLM rerank. Best-effort; fall back to heuristic order on any
    // failure so the user always gets results.
    let order = match llm_rank(&state, &query, &candidates).await {
        Ok(order) => order,
        Err(e) => {
            tracing::info!("rerank: gateway rerank failed, using heuristic order: {e}");
            (0..candidates.len()).collect()
        }
    };

    Ok(apply_rank_order(candidates, &order, k))
}

/// Make the single gateway call and parse its answer into a permutation of
/// `0..candidates.len()`. Returns Err on transport/timeout; the caller decides
/// the fallback.
async fn llm_rank(
    state: &State<'_, AppState>,
    query: &str,
    candidates: &[RetrievalHit],
) -> Result<Vec<usize>, String> {
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let user = build_rerank_prompt(query, candidates);

    let client = GatewayClient::new(cfg.gateway_base_url.clone(), api_key);
    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: RERANK_SYSTEM.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user,
            },
        ],
        stream: true,
        temperature: Some(0.0),
    };

    let body = run_rerank_pass(client, req).await?;
    Ok(parse_rank_order(&body, candidates.len()))
}

/// Run one streaming gateway pass and collect the full text body, bounded by
/// [`RERANK_TIMEOUT`].
async fn run_rerank_pass(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
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

    match tokio::time::timeout(RERANK_TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!(
            "rerank pass timed out after {}s",
            RERANK_TIMEOUT.as_secs()
        )),
    }
}
