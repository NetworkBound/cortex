//! One-shot completion routed through the adapter registry.
//!
//! The "single LLM call" primitive for helper features (the eval harness
//! first): takes a model slug exactly as the composer's model picker produces
//! it (`claude-…`, `gpt-…`, `ollama:tag`), routes it with the SAME
//! `orchestrator::route` the chat path uses, and collects the streamed tokens
//! into one String. Unlike the per-feature `GatewayClient` copies this
//! replaces, a caller here can reach ANY registered adapter — which is what
//! lets the eval harness benchmark a model the Cookbook just pulled instead
//! of being hardwired to the gateway config.

use super::adapter::{AgentAdapter, AgentEvent, ChatRequest};
use super::registry::Registry;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn request(model: Option<&str>, prompt: &str) -> ChatRequest {
    ChatRequest {
        session_id: format!("oneshot-{}", uuid::Uuid::new_v4()),
        message: prompt.to_string(),
        project_root: None,
        history: Vec::new(),
        model: model.map(|m| m.to_string()),
        reasoning_effort: None,
    }
}

/// Resolve which adapter serves `model` (`None` → the default route), the same
/// way the chat path does. Returns the adapter plus its registry id. The read
/// guard never crosses an await — callers can hold the result across one.
pub fn resolve_adapter(
    registry: &RwLock<Registry>,
    model: Option<&str>,
) -> Result<(Arc<dyn AgentAdapter>, String), String> {
    let req = request(model, "");
    let reg = registry.read();
    let decision = crate::orchestrator::route(&req, &reg, None);
    let id = decision
        .agents
        .first()
        .cloned()
        .ok_or("no agent adapter available")?;
    let adapter = reg
        .get(&id)
        .ok_or_else(|| format!("agent `{id}` is not registered"))?;
    Ok((adapter, id))
}

/// Run one prompt through `adapter` and collect the streamed tokens into a
/// single String. Adapter-reported `Error` events (and run failures) become
/// `Err` only when no usable text arrived — a model that streamed an answer
/// and then hiccuped still counts.
pub async fn collect_completion(
    adapter: Arc<dyn AgentAdapter>,
    model: Option<String>,
    prompt: String,
) -> Result<String, String> {
    let req = request(model.as_deref(), &prompt);
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let run = adapter.run(req, tx);
    let collect = async {
        let mut buf = String::new();
        let mut err: Option<String> = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Token { delta } => buf.push_str(&delta),
                AgentEvent::Error { message } => err = Some(message),
                AgentEvent::Done { .. } => break,
                _ => {}
            }
        }
        (buf, err)
    };
    let (run_res, (buf, err)) = tokio::join!(run, collect);
    if buf.trim().is_empty() {
        if let Some(e) = err {
            return Err(e);
        }
        if let Err(e) = run_res {
            return Err(format!("agent run failed: {e}"));
        }
        return Err("the model returned an empty response".into());
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Resilience: retry-with-backoff on transient provider blips + a model
// fallback chain. A single transient hiccup (429/503/timeout/connection reset)
// used to surface straight to the user as a failed eval task, a dropped inline
// rewrite, or a wedged team worker; now the helper features self-heal — they
// retry the SAME model a few times with exponential backoff, then fall through
// to the next model in a configured chain before giving up.
// ---------------------------------------------------------------------------

/// How many times to retry one model, and the backoff envelope between tries.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Attempts PER model (clamped to ≥1). `3` ⇒ the initial call + 2 retries.
    pub max_attempts: u32,
    /// Backoff before the first retry; doubles each subsequent retry.
    pub base_delay_ms: u64,
    /// Cap so a long chain can't sleep for minutes.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 3, base_delay_ms: 300, max_delay_ms: 4_000 }
    }
}

impl RetryPolicy {
    /// Exponential backoff before the `attempt`-th retry (1-based): the wait
    /// after attempt 1 is `base`, after attempt 2 `2·base`, … capped at `max`.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        let ms = self
            .base_delay_ms
            .saturating_mul(1u64 << shift)
            .min(self.max_delay_ms);
        Duration::from_millis(ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// A blip worth retrying the same model for (rate limit, overload, 5xx,
    /// network/timeout).
    Transient,
    /// A deterministic failure (auth, bad request, model-not-found, empty
    /// answer) — retrying the same model just burns time/quota. The chain still
    /// advances to the NEXT model, which may well succeed.
    Permanent,
}

/// Classify an adapter error string. We only have the humanized message (the
/// adapters collapse status codes into text), so this is substring-based and
/// deliberately conservative: an UNKNOWN error is treated as `Permanent` so we
/// never hammer a provider over a genuine logic/auth failure — the fallback
/// chain is the escape hatch, not blind same-model retries.
pub fn classify_error(msg: &str) -> ErrorClass {
    let m = msg.to_ascii_lowercase();
    // Permanent wins when both could match (an auth error that mentions
    // "connection" is still auth).
    const PERMANENT: &[&str] = &[
        "401", "403", "unauthorized", "forbidden", "invalid api key", "invalid_api_key",
        "authentication", "permission", "400", "invalid request", "bad request",
        "model not found", "model_not_found", "404", "does not exist", "not found",
        "unsupported", "context length", "maximum context", "too long",
    ];
    const TRANSIENT: &[&str] = &[
        "429", "rate limit", "rate_limit", "overloaded", "over capacity", "capacity",
        "timed out", "timeout", "temporarily", "try again", "500", "502", "503", "504",
        "internal server error", "bad gateway", "service unavailable", "gateway timeout",
        "connection", "reset", "broken pipe", "unexpected eof", "dns", "network",
        "unreachable", "connect error",
    ];
    if PERMANENT.iter().any(|p| m.contains(p)) {
        return ErrorClass::Permanent;
    }
    if TRANSIENT.iter().any(|t| m.contains(t)) {
        return ErrorClass::Transient;
    }
    ErrorClass::Permanent
}

/// The result of a resilient completion: the text plus which model actually
/// answered (so a caller that fell back can label the turn honestly) and how
/// many attempts it took.
#[derive(Debug, Clone)]
pub struct CompletionOutcome {
    pub text: String,
    /// The model slug that produced the answer (`None` = the default route).
    pub model: Option<String>,
    /// The registry id of the adapter that answered.
    pub agent_id: String,
    /// Total `collect_completion` calls across every model tried.
    pub attempts: u32,
    /// `true` when a non-primary model in the chain produced the answer.
    pub fell_back: bool,
}

/// Build the model fallback chain for `primary`: the primary first, then any
/// slugs in `CORTEX_MODEL_FALLBACKS` (comma-separated), de-duplicated. With no
/// env configured the chain is just `[primary]` — retry-only, no surprise
/// provider switch. `None` primary = the default route.
pub fn fallback_chain(primary: Option<String>) -> Vec<Option<String>> {
    let mut chain = vec![primary];
    if let Ok(raw) = std::env::var("CORTEX_MODEL_FALLBACKS") {
        for slug in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let m = Some(slug.to_string());
            if !chain.contains(&m) {
                chain.push(m);
            }
        }
    }
    chain
}

/// Core retry/fallback loop, generic over how a model slug resolves to an
/// adapter so it's unit-testable without standing up the whole registry/router.
/// Each model gets up to `policy.max_attempts` tries (retrying only on
/// `Transient` errors, with backoff); a model that exhausts its retries OR
/// fails permanently drops through to the next in `chain`.
async fn run_chain<F>(
    chain: &[Option<String>],
    prompt: &str,
    policy: &RetryPolicy,
    mut resolve: F,
) -> Result<CompletionOutcome, String>
where
    F: FnMut(Option<&str>) -> Result<(Arc<dyn AgentAdapter>, String), String>,
{
    let owned: Vec<Option<String>> = if chain.is_empty() { vec![None] } else { chain.to_vec() };
    let mut attempts = 0u32;
    let mut last_err = "no model produced a completion".to_string();

    for (idx, model) in owned.iter().enumerate() {
        let (adapter, agent_id) = match resolve(model.as_deref()) {
            Ok(v) => v,
            Err(e) => {
                // Can't even resolve an adapter for this model → try the next.
                last_err = e;
                continue;
            }
        };
        let max = policy.max_attempts.max(1);
        for attempt in 1..=max {
            attempts += 1;
            match collect_completion(adapter.clone(), model.clone(), prompt.to_string()).await {
                Ok(text) => {
                    return Ok(CompletionOutcome {
                        text,
                        model: model.clone(),
                        agent_id,
                        attempts,
                        fell_back: idx > 0,
                    });
                }
                Err(e) => {
                    let transient = classify_error(&e) == ErrorClass::Transient;
                    last_err = e;
                    if transient && attempt < max {
                        let delay = policy.backoff(attempt);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        continue; // retry the same model
                    }
                    break; // permanent, or out of retries → next model
                }
            }
        }
    }

    Err(last_err)
}

/// Run a model `chain` through the registry with retry + fallback. Resolves
/// each model with the SAME routing the chat path uses (re-resolving per model
/// so a fallback Ollama slug reaches the Ollama adapter, etc.).
pub async fn complete_with_fallback(
    registry: &RwLock<Registry>,
    chain: &[Option<String>],
    prompt: &str,
    policy: &RetryPolicy,
) -> Result<CompletionOutcome, String> {
    run_chain(chain, prompt, policy, |m| resolve_adapter(registry, m)).await
}

/// The one-call entry helper features use: resolve `primary`'s configured
/// fallback chain and run it with the default retry policy, so a transient
/// provider blip self-heals instead of failing the feature.
pub async fn complete_resilient(
    registry: &RwLock<Registry>,
    primary: Option<String>,
    prompt: String,
) -> Result<CompletionOutcome, String> {
    let chain = fallback_chain(primary);
    complete_with_fallback(registry, &chain, &prompt, &RetryPolicy::default()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::{AgentCapability, AgentDescriptor};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stub that streams fixed deltas, optionally an error event, then Done.
    struct StubAdapter {
        deltas: Vec<&'static str>,
        error: Option<&'static str>,
    }

    #[async_trait]
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: "stub".into(),
                label: "stub".into(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(
            &self,
            _req: ChatRequest,
            tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            for d in &self.deltas {
                let _ = tx.send(AgentEvent::Token { delta: (*d).to_string() }).await;
            }
            if let Some(e) = self.error {
                let _ = tx.send(AgentEvent::Error { message: e.to_string() }).await;
            }
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn collects_streamed_tokens() {
        let a = Arc::new(StubAdapter { deltas: vec!["Hello", ", ", "world"], error: None });
        let out = collect_completion(a, None, "hi".into()).await.unwrap();
        assert_eq!(out, "Hello, world");
    }

    #[tokio::test]
    async fn error_with_no_text_is_err() {
        let a = Arc::new(StubAdapter { deltas: vec![], error: Some("boom") });
        let err = collect_completion(a, None, "hi".into()).await.unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn text_survives_late_error() {
        let a = Arc::new(StubAdapter { deltas: vec!["partial answer"], error: Some("hiccup") });
        let out = collect_completion(a, None, "hi".into()).await.unwrap();
        assert_eq!(out, "partial answer");
    }

    #[tokio::test]
    async fn empty_stream_is_err() {
        let a = Arc::new(StubAdapter { deltas: vec![], error: None });
        assert!(collect_completion(a, None, "hi".into()).await.is_err());
    }

    // --- resilience: classification, backoff, retry + fallback ---------------

    /// Fails (transient or permanent) for the first `fail_times` calls, then
    /// streams `answer`. Call count persists across retries because the same
    /// Arc is reused for a given model.
    struct FlakyAdapter {
        fail_times: usize,
        calls: AtomicUsize,
        err_msg: &'static str,
        answer: &'static str,
    }

    #[async_trait]
    impl AgentAdapter for FlakyAdapter {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: "flaky".into(),
                label: "flaky".into(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(
            &self,
            _req: ChatRequest,
            tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_times {
                let _ = tx.send(AgentEvent::Error { message: self.err_msg.to_string() }).await;
            } else {
                let _ = tx.send(AgentEvent::Token { delta: self.answer.to_string() }).await;
            }
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            Ok(())
        }
    }

    fn instant_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy { max_attempts, base_delay_ms: 0, max_delay_ms: 0 }
    }

    #[test]
    fn classifies_transient_and_permanent() {
        use ErrorClass::*;
        assert_eq!(classify_error("HTTP 429 Too Many Requests"), Transient);
        assert_eq!(classify_error("upstream is Overloaded, try again"), Transient);
        assert_eq!(classify_error("503 Service Unavailable"), Transient);
        assert_eq!(classify_error("connection reset by peer"), Transient);
        assert_eq!(classify_error("request timed out"), Transient);
        assert_eq!(classify_error("401 Unauthorized: invalid api key"), Permanent);
        assert_eq!(classify_error("model not found: claude-retired"), Permanent);
        assert_eq!(classify_error("400 invalid request"), Permanent);
        // Auth wins even when the text also mentions a transient-ish word.
        assert_eq!(classify_error("unauthorized (connection refused after)"), Permanent);
        // Unknown → conservative Permanent (don't blind-retry).
        assert_eq!(classify_error("the model returned an empty response"), Permanent);
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let p = RetryPolicy { max_attempts: 5, base_delay_ms: 100, max_delay_ms: 350 };
        assert_eq!(p.backoff(1), Duration::from_millis(100));
        assert_eq!(p.backoff(2), Duration::from_millis(200));
        assert_eq!(p.backoff(3), Duration::from_millis(350)); // 400 capped → 350
        assert_eq!(p.backoff(9), Duration::from_millis(350)); // no overflow, capped
    }

    #[tokio::test]
    async fn retries_same_model_through_transient_blips() {
        // Fails twice (transient) then succeeds on the 3rd try — one model.
        let a = Arc::new(FlakyAdapter {
            fail_times: 2,
            calls: AtomicUsize::new(0),
            err_msg: "503 service unavailable",
            answer: "recovered",
        }) as Arc<dyn AgentAdapter>;
        let resolve = |_m: Option<&str>| Ok((a.clone(), "flaky".to_string()));
        let out = run_chain(&[None], "hi", &instant_policy(3), resolve).await.unwrap();
        assert_eq!(out.text, "recovered");
        assert_eq!(out.attempts, 3);
        assert!(!out.fell_back);
    }

    #[tokio::test]
    async fn gives_up_on_same_model_after_max_attempts() {
        let a = Arc::new(FlakyAdapter {
            fail_times: usize::MAX,
            calls: AtomicUsize::new(0),
            err_msg: "429 rate limited",
            answer: "",
        });
        let a_dyn = a.clone() as Arc<dyn AgentAdapter>;
        let resolve = |_m: Option<&str>| Ok((a_dyn.clone(), "flaky".to_string()));
        let err = run_chain(&[None], "hi", &instant_policy(3), resolve).await.unwrap_err();
        assert!(err.contains("429"));
        // Exactly max_attempts calls, no more.
        assert_eq!(a.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn permanent_error_skips_retries_but_falls_back() {
        // Model A fails permanently (auth) — must be tried exactly ONCE — then
        // the chain advances to model B which answers.
        let a = Arc::new(FlakyAdapter {
            fail_times: usize::MAX,
            calls: AtomicUsize::new(0),
            err_msg: "401 unauthorized",
            answer: "",
        });
        let b = Arc::new(FlakyAdapter {
            fail_times: 0,
            calls: AtomicUsize::new(0),
            err_msg: "",
            answer: "from B",
        });
        let a_dyn = a.clone() as Arc<dyn AgentAdapter>;
        let b_dyn = b.clone() as Arc<dyn AgentAdapter>;
        let resolve = |m: Option<&str>| match m {
            Some("a") => Ok((a_dyn.clone(), "a".to_string())),
            _ => Ok((b_dyn.clone(), "b".to_string())),
        };
        let chain = vec![Some("a".to_string()), Some("b".to_string())];
        let out = run_chain(&chain, "hi", &instant_policy(3), resolve).await.unwrap();
        assert_eq!(out.text, "from B");
        assert_eq!(out.model.as_deref(), Some("b"));
        assert!(out.fell_back);
        assert_eq!(a.calls.load(Ordering::SeqCst), 1, "permanent error must not retry A");
    }

    #[tokio::test]
    async fn transient_exhaustion_falls_back_to_next_model() {
        // A always fails transiently (exhausts its retries) → fall back to B.
        let a = Arc::new(FlakyAdapter {
            fail_times: usize::MAX,
            calls: AtomicUsize::new(0),
            err_msg: "connection reset",
            answer: "",
        });
        let b = Arc::new(FlakyAdapter {
            fail_times: 0,
            calls: AtomicUsize::new(0),
            err_msg: "",
            answer: "from B",
        });
        let a_dyn = a.clone() as Arc<dyn AgentAdapter>;
        let b_dyn = b.clone() as Arc<dyn AgentAdapter>;
        let resolve = |m: Option<&str>| match m {
            Some("a") => Ok((a_dyn.clone(), "a".to_string())),
            _ => Ok((b_dyn.clone(), "b".to_string())),
        };
        let chain = vec![Some("a".to_string()), Some("b".to_string())];
        let out = run_chain(&chain, "hi", &instant_policy(2), resolve).await.unwrap();
        assert_eq!(out.text, "from B");
        assert!(out.fell_back);
        assert_eq!(a.calls.load(Ordering::SeqCst), 2, "A retried up to its cap then yielded");
        assert_eq!(out.attempts, 3, "2 on A + 1 on B");
    }

    #[tokio::test]
    async fn unresolvable_model_is_skipped() {
        let b = Arc::new(FlakyAdapter {
            fail_times: 0,
            calls: AtomicUsize::new(0),
            err_msg: "",
            answer: "from B",
        });
        let b_dyn = b.clone() as Arc<dyn AgentAdapter>;
        let resolve = |m: Option<&str>| match m {
            Some("a") => Err("agent `a` is not registered".to_string()),
            _ => Ok((b_dyn.clone(), "b".to_string())),
        };
        let chain = vec![Some("a".to_string()), Some("b".to_string())];
        let out = run_chain(&chain, "hi", &instant_policy(2), resolve).await.unwrap();
        assert_eq!(out.text, "from B");
        assert!(out.fell_back);
    }

    #[test]
    fn fallback_chain_dedups_and_keeps_primary_first() {
        // No env set in the default test env → chain is just [primary].
        let chain = fallback_chain(Some("claude-opus-4-8".to_string()));
        assert_eq!(chain, vec![Some("claude-opus-4-8".to_string())]);
    }

    /// LIVE: a failing primary falls back to a REAL local Ollama model that
    /// returns a REAL completion — exercises routing + fallback against an
    /// actual provider. Run with: `cargo test --lib -- --ignored live_fallback`.
    #[tokio::test]
    #[ignore]
    async fn live_fallback_to_real_ollama() {
        use crate::agents::ollama::OllamaAgent;
        let flaky = Arc::new(FlakyAdapter {
            fail_times: usize::MAX,
            calls: AtomicUsize::new(0),
            err_msg: "503 service unavailable",
            answer: "",
        }) as Arc<dyn AgentAdapter>;
        let ollama = Arc::new(OllamaAgent::new(
            "http://localhost:11434".to_string(),
            "llama3.2:1b".to_string(),
        )) as Arc<dyn AgentAdapter>;
        let resolve = |m: Option<&str>| match m {
            Some("flaky-primary") => Ok((flaky.clone(), "flaky".to_string())),
            _ => Ok((ollama.clone(), "ollama".to_string())),
        };
        let chain = vec![
            Some("flaky-primary".to_string()),
            Some("ollama:llama3.2:1b".to_string()),
        ];
        let out = run_chain(
            &chain,
            "Reply with exactly one word: pong",
            &instant_policy(2),
            resolve,
        )
        .await
        .expect("fallback to real ollama should produce a completion");
        assert!(out.fell_back, "the primary failed → must have fallen back");
        assert_eq!(out.model.as_deref(), Some("ollama:llama3.2:1b"));
        assert!(!out.text.trim().is_empty(), "real model returned text");
    }

    #[test]
    fn resolve_routes_model_slug_through_registry() {
        // ollama: slug routes to the ollama adapter when registered+available;
        // anything else falls back to the default route — same behavior the
        // chat path gets from orchestrator::route.
        let mut reg = Registry::new();
        reg.register(Arc::new(StubAdapter { deltas: vec![], error: None })); // id "stub"
        let reg = RwLock::new(reg);
        let (_, id) = resolve_adapter(&reg, Some("anything")).unwrap();
        // No gateway-remote/ollama registered → route falls back to the only
        // available adapter rather than erroring.
        assert_eq!(id, "stub");
    }
}
