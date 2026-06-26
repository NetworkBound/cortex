//! CrewAI-style **manager process** — auto-decompose a high-level goal into
//! role-tagged subtasks, run each through the gateway using the assigned role's
//! system prompt, and validate each output before advancing.
//!
//! Lifecycle (all commands take a `plan_id` produced by `manager_decompose`):
//!   1. `manager_decompose(goal)` — manager LLM returns an ordered plan of
//!      subtasks. Stored in the in-memory registry keyed by ULID `plan_id`.
//!   2. `manager_run_step(plan_id, step_index)` — runs the subtask via the
//!      assigned role's system prompt + the goal/prior-output context, then
//!      auto-validates the output. Returns `{ output, validation }`.
//!   3. `manager_validate(plan_id, step_index, output)` — standalone validate
//!      hook for the UI when run_step's auto-validation is skipped.
//!
//! Plans are NOT persisted to disk in v1 — they live in a `parking_lot::Mutex`
//! HashMap with a 1h TTL. The registry is pruned opportunistically on every
//! read so we never need a background task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::mpsc;

use crate::agents::roles;
use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Wall-clock budget for the manager decomposition call. Decomposition only
/// asks for a small JSON payload so 45s is generous.
const DECOMPOSE_TIMEOUT: Duration = Duration::from_secs(45);

/// Per-step worker timeout. Some subtasks (e.g. test-writer) emit longer
/// outputs than commit-msg style calls; 180s matches the batch runner ceiling.
const STEP_TIMEOUT: Duration = Duration::from_secs(180);

/// Validation calls return a tiny JSON object — 30s is plenty.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Plans live in memory for an hour after the last access. Long enough for a
/// user to step through a multi-stage plan in the modal; short enough that a
/// forgotten plan doesn't pin memory forever.
const PLAN_TTL: Duration = Duration::from_secs(60 * 60);

/// Hard ceiling on subtasks per plan. Above this the manager is almost
/// certainly hallucinating, and the modal scroll becomes unusable anyway.
const MAX_SUBTASKS: usize = 20;

const MANAGER_SYSTEM_PROMPT: &str = "You are a project manager. Given the goal \
below and the available specialist roles, decompose the work into an ordered \
list of subtasks. Each subtask MUST be assignable to exactly one of the \
provided roles (use the role's exact `name`). Return ONLY valid JSON with this \
schema, no prose, no fences:\n\
{\n  \"subtasks\": [\n    { \"name\": \"short title\", \"role\": \"role-name\", \"prompt\": \"detailed instructions for the specialist\", \"depends_on\": [/* indices of prior subtasks this depends on */] }\n  ]\n}\n\
Order subtasks so dependencies come first. Keep the list focused (3-8 subtasks \
is typical, max 20). If no roles are available, return `{\"subtasks\": []}`.";

const VALIDATOR_SYSTEM_PROMPT: &str = "You are validating a subtask output. \
Did the output achieve the subtask? Be lenient — partial successes are still \
'ok' as long as the core deliverable is present. Return ONLY valid JSON with \
this schema, no prose, no fences:\n\
{ \"ok\": true|false, \"reason\": \"one-sentence justification\" }";

// ── Wire types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub name: String,
    pub role: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<usize>,
    /// "pending" | "running" | "validating" | "done" | "failed"
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

fn default_status() -> String { "pending".into() }

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub plan_id: String,
    pub goal: String,
    pub subtasks: Vec<Subtask>,
    pub created_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Validation {
    pub ok: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub output: String,
    pub validation: Validation,
}

/// Shape returned by the manager LLM when we ask for a decomposition. We
/// deserialise into this and then promote to `Plan` with status/output filled.
#[derive(Debug, Deserialize)]
struct DecomposeResponse {
    #[serde(default)]
    subtasks: Vec<DecomposeSubtask>,
}

#[derive(Debug, Deserialize)]
struct DecomposeSubtask {
    name: String,
    role: String,
    prompt: String,
    #[serde(default)]
    depends_on: Vec<usize>,
}

// ── In-memory registry ─────────────────────────────────────────────────────

struct StoredPlan {
    plan: Plan,
    last_touch: Instant,
}

static REGISTRY: Lazy<Arc<Mutex<HashMap<String, StoredPlan>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Prune any plans older than `PLAN_TTL`. Cheap O(n) sweep called on every
/// read — keeps the registry from leaking without a background task.
fn prune(reg: &mut HashMap<String, StoredPlan>) {
    let cutoff = Instant::now();
    reg.retain(|_, sp| cutoff.duration_since(sp.last_touch) < PLAN_TTL);
}

fn store_plan(plan: Plan) {
    let mut reg = REGISTRY.lock();
    prune(&mut reg);
    reg.insert(
        plan.plan_id.clone(),
        StoredPlan { plan, last_touch: Instant::now() },
    );
}

fn read_plan(plan_id: &str) -> Option<Plan> {
    let mut reg = REGISTRY.lock();
    prune(&mut reg);
    reg.get_mut(plan_id).map(|sp| {
        sp.last_touch = Instant::now();
        sp.plan.clone()
    })
}

fn update_plan<F: FnOnce(&mut Plan)>(plan_id: &str, f: F) -> Result<Plan, String> {
    let mut reg = REGISTRY.lock();
    prune(&mut reg);
    let sp = reg
        .get_mut(plan_id)
        .ok_or_else(|| format!("plan '{plan_id}' not found (expired or never created)"))?;
    f(&mut sp.plan);
    sp.last_touch = Instant::now();
    Ok(sp.plan.clone())
}

// ── Tauri commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn manager_decompose(
    goal: String,
    state: State<'_, AppState>,
) -> Result<Plan, String> {
    let goal_trim = goal.trim().to_string();
    if goal_trim.is_empty() {
        return Err("goal is required".into());
    }

    let roles = roles::list_roles();
    if roles.is_empty() {
        return Err("no roles available — define at least one under ~/.cortex/roles/".into());
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let role_summary = roles
        .iter()
        .map(|r| {
            let desc = r.description.as_deref().unwrap_or("(no description)");
            format!("- {} — {}", r.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let user_prompt = format!(
        "GOAL:\n{goal_trim}\n\nAVAILABLE ROLES:\n{role_summary}\n\nReturn the JSON plan.",
    );

    let raw = call_gateway(
        &client,
        &cfg.gateway_model,
        MANAGER_SYSTEM_PROMPT,
        &user_prompt,
        Some(0.2),
        DECOMPOSE_TIMEOUT,
    )
    .await?;

    let parsed: DecomposeResponse = parse_json(&raw)
        .map_err(|e| format!("manager returned invalid JSON: {e}\n\nraw:\n{raw}"))?;

    if parsed.subtasks.is_empty() {
        return Err("manager returned no subtasks — try a more concrete goal".into());
    }
    if parsed.subtasks.len() > MAX_SUBTASKS {
        return Err(format!(
            "manager returned {} subtasks (max {MAX_SUBTASKS})",
            parsed.subtasks.len()
        ));
    }

    // Map decomposed shape → wire Subtask with status fields filled.
    let role_names: std::collections::HashSet<&str> =
        roles.iter().map(|r| r.name.as_str()).collect();
    let mut subtasks = Vec::with_capacity(parsed.subtasks.len());
    for (idx, s) in parsed.subtasks.into_iter().enumerate() {
        if !role_names.contains(s.role.as_str()) {
            return Err(format!(
                "manager assigned subtask {idx} to unknown role '{}'",
                s.role
            ));
        }
        for d in &s.depends_on {
            if *d >= idx {
                return Err(format!(
                    "subtask {idx} depends on {d} which is not earlier in the plan"
                ));
            }
        }
        subtasks.push(Subtask {
            name: s.name,
            role: s.role,
            prompt: s.prompt,
            depends_on: s.depends_on,
            status: "pending".into(),
            output: None,
        });
    }

    let plan = Plan {
        plan_id: ulid::Ulid::new().to_string(),
        goal: goal_trim,
        subtasks,
        created_unix_ms: chrono::Utc::now().timestamp_millis(),
    };
    store_plan(plan.clone());
    Ok(plan)
}

#[tauri::command]
pub async fn manager_run_step(
    plan_id: String,
    step_index: usize,
    state: State<'_, AppState>,
) -> Result<StepResult, String> {
    let plan = read_plan(&plan_id)
        .ok_or_else(|| format!("plan '{plan_id}' not found (expired or never created)"))?;
    let subtask = plan
        .subtasks
        .get(step_index)
        .ok_or_else(|| format!("step {step_index} out of range (plan has {} subtasks)", plan.subtasks.len()))?
        .clone();

    // Resolve the role's system prompt. Missing roles fall back to a neutral
    // instruction so the run can still proceed (the manager may have invented
    // a role between decompose + run).
    let role = roles::get_role(&subtask.role);
    let system_prompt = role
        .as_ref()
        .and_then(|r| r.system_prompt.clone())
        .unwrap_or_else(|| format!("You are a '{}' specialist. Complete the task precisely.", subtask.role));

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    // Mark running. The frontend re-polls via subsequent calls; we don't
    // stream progress for v1 to keep the wire surface small.
    let _ = update_plan(&plan_id, |p| {
        if let Some(s) = p.subtasks.get_mut(step_index) {
            s.status = "running".into();
        }
    });

    // Build the user prompt: goal + any upstream outputs + the subtask prompt.
    let mut context = format!("OVERALL GOAL:\n{}\n\n", plan.goal);
    if !subtask.depends_on.is_empty() {
        context.push_str("PRIOR OUTPUTS:\n");
        for dep in &subtask.depends_on {
            if let Some(prev) = plan.subtasks.get(*dep) {
                let body = prev.output.as_deref().unwrap_or("(no output)");
                context.push_str(&format!(
                    "--- Step {dep} ({}) ---\n{body}\n\n",
                    prev.name
                ));
            }
        }
    }
    context.push_str(&format!("YOUR TASK ({}):\n{}", subtask.name, subtask.prompt));

    let run_result = call_gateway(
        &client,
        &cfg.gateway_model,
        &system_prompt,
        &context,
        Some(0.3),
        STEP_TIMEOUT,
    )
    .await;

    let output = match run_result {
        Ok(o) if !o.trim().is_empty() => o,
        Ok(_) => {
            let _ = update_plan(&plan_id, |p| {
                if let Some(s) = p.subtasks.get_mut(step_index) {
                    s.status = "failed".into();
                }
            });
            return Err("step returned an empty output".into());
        }
        Err(e) => {
            let _ = update_plan(&plan_id, |p| {
                if let Some(s) = p.subtasks.get_mut(step_index) {
                    s.status = "failed".into();
                }
            });
            return Err(e);
        }
    };

    // Persist the raw output + flip to "validating" before the validator call
    // so the UI can render the partial state if it polls mid-flight.
    let _ = update_plan(&plan_id, |p| {
        if let Some(s) = p.subtasks.get_mut(step_index) {
            s.output = Some(output.clone());
            s.status = "validating".into();
        }
    });

    let validation = validate_inner(&client, &cfg.gateway_model, &subtask, &output).await?;
    let final_status = if validation.ok { "done" } else { "failed" };
    let _ = update_plan(&plan_id, |p| {
        if let Some(s) = p.subtasks.get_mut(step_index) {
            s.status = final_status.into();
        }
    });

    Ok(StepResult { output, validation })
}

#[tauri::command]
pub async fn manager_validate(
    plan_id: String,
    step_index: usize,
    output: String,
    state: State<'_, AppState>,
) -> Result<Validation, String> {
    let plan = read_plan(&plan_id)
        .ok_or_else(|| format!("plan '{plan_id}' not found (expired or never created)"))?;
    let subtask = plan
        .subtasks
        .get(step_index)
        .ok_or_else(|| format!("step {step_index} out of range (plan has {} subtasks)", plan.subtasks.len()))?
        .clone();

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let validation = validate_inner(&client, &cfg.gateway_model, &subtask, &output).await?;
    let final_status = if validation.ok { "done" } else { "failed" };
    let _ = update_plan(&plan_id, |p| {
        if let Some(s) = p.subtasks.get_mut(step_index) {
            s.status = final_status.into();
            s.output = Some(output.clone());
        }
    });
    Ok(validation)
}

// ── Internals ──────────────────────────────────────────────────────────────

async fn validate_inner(
    client: &GatewayClient,
    model: &str,
    subtask: &Subtask,
    output: &str,
) -> Result<Validation, String> {
    let user_prompt = format!(
        "SUBTASK: {}\n\nINSTRUCTIONS:\n{}\n\nOUTPUT:\n{}",
        subtask.name, subtask.prompt, output
    );
    let raw = call_gateway(
        client,
        model,
        VALIDATOR_SYSTEM_PROMPT,
        &user_prompt,
        Some(0.0),
        VALIDATE_TIMEOUT,
    )
    .await?;

    // Validator is small — be forgiving about wrapping fences.
    let parsed: Validation = parse_json(&raw).unwrap_or_else(|_| Validation {
        ok: false,
        reason: format!("validator returned unparseable JSON: {raw}"),
    });
    Ok(parsed)
}

/// Single-shot gateway call that collects a streaming response into one String.
/// Mirrors the pattern used by `commit_suggest` and `batch_runner`.
async fn call_gateway(
    client: &GatewayClient,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: Option<f32>,
    timeout: Duration,
) -> Result<String, String> {
    let req = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system_prompt.into() },
            ChatMessage { role: "user".into(), content: user_prompt.into() },
        ],
        stream: true,
        temperature,
    };

    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
    let client = client.clone();
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

    match tokio::time::timeout(timeout, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!("The gateway timed out after {}s", timeout.as_secs())),
    }
}

/// Best-effort JSON parser. Strips a leading ```json / ``` fence if present
/// before delegating to serde_json.
fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, String> {
    let trimmed = strip_fence(raw.trim());
    serde_json::from_str(trimmed).map_err(|e| e.to_string())
}

fn strip_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim();
        }
        return rest.trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim();
        }
        return rest.trim();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fence_handles_json_block() {
        let raw = "```json\n{\"ok\": true}\n```";
        assert_eq!(strip_fence(raw), "{\"ok\": true}");
    }

    #[test]
    fn strip_fence_handles_plain_block() {
        let raw = "```\n{\"ok\": false}\n```";
        assert_eq!(strip_fence(raw), "{\"ok\": false}");
    }

    #[test]
    fn strip_fence_passes_through_raw_json() {
        let raw = "{\"ok\": true, \"reason\": \"fine\"}";
        assert_eq!(strip_fence(raw), raw);
    }

    #[test]
    fn parse_json_round_trips_validation() {
        let v: Validation = parse_json("{\"ok\": true, \"reason\": \"good\"}").unwrap();
        assert!(v.ok);
        assert_eq!(v.reason, "good");
    }

    #[test]
    fn registry_round_trip_and_update() {
        let plan = Plan {
            plan_id: "TEST_PLAN_X".into(),
            goal: "demo".into(),
            subtasks: vec![Subtask {
                name: "a".into(),
                role: "code-reviewer".into(),
                prompt: "do a".into(),
                depends_on: vec![],
                status: "pending".into(),
                output: None,
            }],
            created_unix_ms: 0,
        };
        store_plan(plan.clone());
        let fetched = read_plan("TEST_PLAN_X").unwrap();
        assert_eq!(fetched.subtasks.len(), 1);
        let updated = update_plan("TEST_PLAN_X", |p| {
            p.subtasks[0].status = "done".into();
        })
        .unwrap();
        assert_eq!(updated.subtasks[0].status, "done");
        // Cleanup so other tests in this process don't see the leak.
        REGISTRY.lock().remove("TEST_PLAN_X");
    }

    #[test]
    fn update_plan_errors_on_missing_id() {
        let err = update_plan("does-not-exist", |_| {}).unwrap_err();
        assert!(err.contains("not found"));
    }
}
