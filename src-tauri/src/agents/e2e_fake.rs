//! Deterministic fake-LLM adapter for E2E runs — registered ONLY when the
//! app is launched with `CORTEX_E2E=1` (see `lib.rs`), so production builds
//! never expose it. The probe (src/lib/e2e-probe.ts) picks it explicitly via
//! `chat_send`'s `agent` arg, which means a probe turn exercises the REAL
//! pipeline — routing, dispatch, the agent event loop with its focus-chain
//! scanner, the frontend `agent-event:` listener, store mutators and
//! persistence — with only the upstream LLM replaced by a canned stream.
//!
//! Marker contract (matches the established `[[e2e:…]]` convention used by
//! eval_harness/routines/inline_assist):
//!   * `[[e2e:focus-chain]]` — streams a reply containing two ```focus-chain
//!     blocks, deliberately split across token chunks mid-fence so the
//!     scanner's incremental reassembly is what's under test. The final
//!     block has 3 items, all done.
//!   * `[[e2e:err]]` — emits a deterministic `Error` event.
//!   * anything else — echoes the message back.

use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest};
use tokio::sync::mpsc;

/// Chunks for the focus-chain script. The fence opener, body and closer are
/// split across chunk boundaries on purpose (`focus-` / `chain`, ` `` ` /
/// `` ` ``) — a scanner that only looks inside single deltas would miss both
/// blocks.
const FOCUS_CHAIN_CHUNKS: &[&str] = &[
    "Starting the multi-step task.\n\n```focus-",
    "chain\n- [x] Inspect the request\n- [ ] Draft the reply\n- [ ] Final check\n``",
    "`\n\nDrafting…\n\n```focus-chain\n- [x] Inspect the request\n- [x] Draft the reply\n",
    "- [x] Final check\n```\n\nAll steps complete.",
];

/// Recognize the team manager's planning prompt (`build_plan_prompt`) and
/// synthesize a valid one-task-per-worker JSON plan, so the E2E orchestrator
/// team-run flow is deterministic without a live model. Returns `None` for any
/// other message (ordinary chat turns echo as before).
///
/// Worker ids are lifted straight from the roster lines (`- worker_id "<id>":`)
/// so the plan references the team's real workers. With the `[[e2e:team-code]]`
/// marker present, the FIRST worker is tagged `code`/`hard` (the slice-4 lane
/// dispatch path); everyone else is tagged `chat`/`medium`.
fn synthesize_team_plan(message: &str) -> Option<String> {
    if !message.contains("Respond with ONLY a JSON array") {
        return None;
    }
    let needle = "worker_id \"";
    let ids: Vec<&str> = message
        .match_indices(needle)
        .filter_map(|(i, _)| {
            let rest = &message[i + needle.len()..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect();
    if ids.is_empty() {
        return None;
    }
    let code_first = message.contains("[[e2e:team-code]]");
    let entries: Vec<String> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| {
            let (kind, diff) = if n == 0 && code_first {
                ("code", "hard")
            } else {
                ("chat", "medium")
            };
            format!(
                "{{\"worker_id\":\"{id}\",\"task\":\"e2e synthesized task for {id}\",\"kind\":\"{kind}\",\"difficulty\":\"{diff}\"}}"
            )
        })
        .collect();
    Some(format!("[{}]", entries.join(",")))
}

#[derive(Default)]
pub struct E2eFakeAgent;

impl E2eFakeAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl AgentAdapter for E2eFakeAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: "e2e-fake".to_string(),
            label: "E2E fake LLM".to_string(),
            description: "Deterministic canned-stream adapter; only registered when CORTEX_E2E=1."
                .to_string(),
            capabilities: vec![AgentCapability::Chat],
            available: true,
        }
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let _ = tx
            .send(AgentEvent::Started { agent_id: "e2e-fake".to_string(), run_id: None })
            .await;
        if req.message.contains("[[e2e:focus-chain]]") {
            for chunk in FOCUS_CHAIN_CHUNKS {
                let _ = tx.send(AgentEvent::Token { delta: (*chunk).to_string() }).await;
                // Small gap so chunks arrive as distinct deltas like a real stream.
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        } else if let Some(plan) = synthesize_team_plan(&req.message) {
            // Team manager planning prompt → emit a valid JSON plan so the
            // slice-4 team-run flow is deterministic without a live LLM.
            let _ = tx.send(AgentEvent::Token { delta: plan }).await;
        } else if req.message.contains("[[e2e:err]]") {
            let _ = tx
                .send(AgentEvent::Error { message: "e2e: deterministic failure".to_string() })
                .await;
        } else {
            let _ = tx
                .send(AgentEvent::Token { delta: format!("e2e-echo: {}", req.message) })
                .await;
        }
        let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_prompt(ids: &[&str], marker: bool) -> String {
        let mut roster = String::new();
        for id in ids {
            roster.push_str(&format!("- worker_id \"{id}\": role \"coder\"\n"));
        }
        let m = if marker { " [[e2e:team-code]]" } else { "" };
        format!(
            "You are the manager.\nTeam goal: do it{m}\n\nYour workers:\n{roster}\n\
             Respond with ONLY a JSON array — no prose, no code fences:\n\
             [{{\"worker_id\": \"<id from the roster>\", \"task\": \"<the task>\"}}]"
        )
    }

    #[test]
    fn non_plan_message_is_not_synthesized() {
        assert!(synthesize_team_plan("just a chat turn").is_none());
        // A planning-shaped prompt with no roster ids also declines.
        assert!(synthesize_team_plan("Respond with ONLY a JSON array").is_none());
    }

    #[test]
    fn synthesizes_chat_plan_for_each_roster_worker() {
        let raw = plan_prompt(&["wkr-a", "wkr-b"], false);
        let plan = synthesize_team_plan(&raw).expect("plan synthesized");
        // Parses as a real JSON array, references both ids, all chat/medium —
        // crucially NOT the literal template id `<id from the roster>`.
        assert!(plan.contains("\"wkr-a\"") && plan.contains("\"wkr-b\""));
        assert!(!plan.contains("from the roster"));
        assert!(plan.matches("\"chat\"").count() == 2);
        let v: serde_json::Value = serde_json::from_str(&plan).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn marker_tags_first_worker_as_code() {
        let raw = plan_prompt(&["wkr-a", "wkr-b"], true);
        let plan = synthesize_team_plan(&raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&plan).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["worker_id"], "wkr-a");
        assert_eq!(arr[0]["kind"], "code");
        assert_eq!(arr[0]["difficulty"], "hard");
        assert_eq!(arr[1]["kind"], "chat");
    }
}
