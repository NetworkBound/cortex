//! Cortex Gateway adapter — primary (and now only) agent in Cortex.
//!
//! Uses `/v1/runs` + SSE event stream so we get tool progress events,
//! reasoning, and approval gates rather than the plain
//! `/v1/chat/completions` text-only stream.
//!
//! Important: API key is re-read from the OS keychain on **every** run, so
//! that updating the key via Settings takes effect immediately without
//! requiring an app restart or registry re-registration.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use crate::app_state::AppState;
use crate::gateway::client::{ChatMessage, GatewayClient, RunRequest, RunStreamItem};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct GatewayRemoteAgent {
    base_url: Arc<RwLock<String>>,
    model_hint: Arc<RwLock<String>>,
}

impl GatewayRemoteAgent {
    pub fn new(
        base_url: impl Into<String>,
        _api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: Arc::new(RwLock::new(base_url.into())),
            model_hint: Arc::new(RwLock::new(model.into())),
        }
    }

    fn current_client(&self) -> GatewayClient {
        let api_key = AppState::get_gateway_api_key().unwrap_or_default();
        GatewayClient::new(self.base_url.read().clone(), api_key)
    }

    pub fn update_base_url(&self, url: String) {
        *self.base_url.write() = url;
    }

    pub fn update_model(&self, model: String) {
        *self.model_hint.write() = model;
    }
}

#[async_trait::async_trait]
impl AgentAdapter for GatewayRemoteAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: "gateway-remote".to_string(),
            label: "Cortex Gateway".to_string(),
            description: format!(
                "Cortex Gateway at {} (fans out to Claude / Codex / Gemini / Ollama via its credential_pool). Model: {}",
                self.base_url.read(),
                self.model_hint.read(),
            ),
            capabilities: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::ShellExec,
                AgentCapability::LongContext,
                AgentCapability::Approval,
            ],
            // Unconfigured gateway (no URL via env / ~/.cortex/infra.json /
            // Settings) → advertise unavailable so routing and the picker
            // skip it instead of dialing nothing.
            available: !self.base_url.read().trim().is_empty(),
        }
    }

    async fn health_check(&self) -> bool {
        self.current_client().health().await
    }

    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let client = self.current_client();

        if self.base_url.read().trim().is_empty() {
            let _ = tx.send(AgentEvent::Started { agent_id: "gateway-remote".into(), run_id: None }).await;
            let _ = tx.send(AgentEvent::Error {
                message: "Cortex Gateway is not configured. Open Settings → Connection and enter your gateway URL (or set CORTEX_GATEWAY_BASE_URL / gateway_base_url in ~/.cortex/infra.json).".into(),
            }).await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        }

        if client.api_key.is_empty() {
            let _ = tx.send(AgentEvent::Started { agent_id: "gateway-remote".into(), run_id: None }).await;
            let _ = tx.send(AgentEvent::Error {
                message: "No Cortex Gateway API key set. Open Settings → paste your Bearer key → Save.".into(),
            }).await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        }

        let session_key = req.project_root.as_ref().map(|p| {
            let s = p.display().to_string();
            // Map the two path separators to distinct sanitized tokens so
            // distinct project roots can't collide onto one session key.
            format!("project:{}", s.replace('/', "-S-").replace('\\', "-B-"))
        });

        let history: Vec<ChatMessage> = req
            .history
            .iter()
            .map(|t| ChatMessage { role: t.role.clone(), content: t.content.clone() })
            .collect();

        // Per-call model override wins over the adapter's configured hint —
        // chat.rs uses this to run architect mode's planner / editor phases
        // against two different upstream models in one session.
        let model_override = req
            .model
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let hint = self.model_hint.read().trim().to_string();
                if hint.is_empty() { None } else { Some(hint) }
            });

        let run_req = RunRequest {
            input: req.message.clone(),
            instructions: None,
            previous_response_id: None,
            conversation_history: if history.is_empty() { None } else { Some(history) },
            model: model_override,
            // Already normalized to a canonical level (or None) by
            // `orchestrator::reasoning::resolve` in chat.rs.
            reasoning_effort: req
                .reasoning_effort
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            cortex_worktree: None,
        };

        let run_id = match client
            .start_run(run_req, Some(&req.session_id), session_key.as_deref())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.send(AgentEvent::Started { agent_id: "gateway-remote".into(), run_id: None }).await;
                let detail = format!("{e}");
                let hint = if detail.contains("401") {
                    " — your API key is invalid or expired. Open Settings to re-enter."
                } else if detail.contains("connect") || detail.contains("dns") {
                    " — gateway unreachable. Check the gateway URL in Settings → Connection."
                } else { "" };
                let _ = tx.send(AgentEvent::Error { message: format!("start_run: {detail}{hint}") }).await;
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                return Ok(());
            }
        };

        let _ = tx
            .send(AgentEvent::Started { agent_id: "gateway-remote".into(), run_id: Some(run_id.clone()) })
            .await;

        let (item_tx, mut item_rx) = mpsc::channel::<RunStreamItem>(128);
        let client_for_sub = client.clone();
        let run_id_clone = run_id.clone();
        let sub_task = tokio::spawn(async move {
            client_for_sub.run_event_stream(&run_id_clone, item_tx).await
        });

        while let Some(item) = item_rx.recv().await {
            match item {
                RunStreamItem::Started { .. } => {}
                RunStreamItem::Delta(t) => {
                    let _ = tx.send(AgentEvent::Token { delta: t }).await;
                }
                RunStreamItem::Reasoning(t) => {
                    let _ = tx.send(AgentEvent::Reasoning { text: t }).await;
                }
                RunStreamItem::ToolStarted { tool, preview } => {
                    let _ = tx.send(AgentEvent::ToolCall {
                        name: tool,
                        args: serde_json::Value::Null,
                        preview,
                    }).await;
                }
                RunStreamItem::ToolCompleted { tool, duration_s, error } => {
                    let _ = tx.send(AgentEvent::ToolResult {
                        name: tool,
                        ok: !error,
                        summary: String::new(),
                        duration_ms: {
                            let ms = duration_s * 1000.0;
                            // NaN -> 0, +/-Inf and out-of-range floats clamp to
                            // i64 bounds instead of saturating to garbage.
                            Some(if ms.is_nan() {
                                0
                            } else {
                                ms.clamp(i64::MIN as f64, i64::MAX as f64) as i64
                            })
                        },
                    }).await;
                }
                RunStreamItem::ApprovalRequest { tool, preview, choices, raw } => {
                    let _ = tx.send(AgentEvent::ApprovalRequest {
                        run_id: run_id.clone(),
                        tool,
                        preview,
                        choices,
                        request: raw,
                    }).await;
                }
                RunStreamItem::ApprovalResponded { choice } => {
                    let _ = tx.send(AgentEvent::ApprovalResolved {
                        run_id: run_id.clone(),
                        choice,
                    }).await;
                }
                RunStreamItem::Done => break,
                RunStreamItem::Status(_) | RunStreamItem::Raw(_) => {}
            }
        }

        // Surface SSE stream failures instead of silently reporting a clean
        // Done. `sub_task` is a JoinHandle<anyhow::Result<()>>, so both the
        // join error (panic/cancel) and the inner stream error must be checked.
        match sub_task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = tx
                    .send(AgentEvent::Error { message: format!("event stream: {e}") })
                    .await;
            }
            Err(join_err) => {
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!("event stream task failed: {join_err}"),
                    })
                    .await;
            }
        }

        let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: Some(run_id) }).await;
        Ok(())
    }
}
