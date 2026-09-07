//! Integration tests for backend logic that doesn't require a Tauri runtime.

#[test]
fn redaction_filters_secrets_and_long_strings() {
    use cortex_lib::observability::sentry::redact;
    let mut v = serde_json::json!({
        "message": "user said something",
        "context": "Bearer sk-abc123secret456789012345678901234",
        "ok": true,
        "long": "x".repeat(400),
        "short_ok": "fine",
    });
    redact(&mut v);
    assert_eq!(v["message"], "[REDACTED]");
    assert_eq!(v["context"], "[REDACTED]");
    assert_eq!(v["long"], "[REDACTED]");
    assert_eq!(v["short_ok"], "fine");
    assert_eq!(v["ok"], true);
}

#[test]
fn orchestrator_honors_explicit_pick() {
    use cortex_lib::agents::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest, Registry};
    use cortex_lib::orchestrator::route;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct Stub(&'static str);
    #[async_trait::async_trait]
    impl AgentAdapter for Stub {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: self.0.into(),
                label: self.0.into(),
                description: "".into(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool { true }
        async fn run(&self, _: ChatRequest, _: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> { Ok(()) }
    }

    let mut reg = Registry::new();
    reg.register(Arc::new(Stub("alpha")));
    reg.register(Arc::new(Stub("beta")));

    let req = ChatRequest {
        session_id: "s1".into(),
        message: "hello".into(),
        project_root: None,
        history: vec![],
        model: None,
        reasoning_effort: None,
    };
    let d = route(&req, &reg, Some("beta".into()));
    assert_eq!(d.agents, vec!["beta"]);
}

#[test]
fn orchestrator_defaults_to_gateway_remote() {
    use cortex_lib::agents::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest, Registry};
    use cortex_lib::orchestrator::route;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct Stub(&'static str);
    #[async_trait::async_trait]
    impl AgentAdapter for Stub {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor { id: self.0.into(), label: self.0.into(), description: "".into(), capabilities: vec![AgentCapability::Chat], available: true }
        }
        async fn health_check(&self) -> bool { true }
        async fn run(&self, _: ChatRequest, _: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> { Ok(()) }
    }

    let mut reg = Registry::new();
    reg.register(Arc::new(Stub("gateway-remote")));
    let req = ChatRequest {
        session_id: "s1".into(),
        message: "anything".into(),
        project_root: None,
        history: vec![],
        model: None,
        reasoning_effort: None,
    };
    let d = route(&req, &reg, None);
    assert_eq!(d.agents, vec!["gateway-remote"]);
}

#[test]
fn memory_markdown_parses_frontmatter() {
    use cortex_lib::memory::markdown::read_entry;
    let tmp = std::env::temp_dir().join("cortex-test-memory.md");
    std::fs::write(&tmp, "---\nname: test\ntype: project\n---\n\n# Hello\n\nBody [[other]] here.\n").unwrap();
    let entry = read_entry(&tmp).unwrap();
    assert_eq!(entry.title.as_deref(), Some("test"));
    assert!(entry.body.contains("Body"));
    assert_eq!(entry.wikilinks, vec!["other"]);
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn tracing_store_records_and_reads() {
    use cortex_lib::observability::tracing_store::TracingStore;
    let store = TracingStore::in_memory();
    store.record_chat_turn("trace1", "sess1", "hi", &["gateway-remote".into()]).unwrap();
    store.start_agent_run("span1", "trace1", "sess1", "gateway-remote", None).unwrap();
    store.finish_agent_run("span1").unwrap();
    let traces = store.recent_traces(10).unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].trace_id, "trace1");
    assert!(traces[0].spans.iter().any(|s| s.name == "agent.run"));
}

#[test]
fn issue_dedup_works() {
    use cortex_lib::agents::AgentEvent;
    use cortex_lib::observability::tracing_store::TracingStore;
    let store = TracingStore::in_memory();
    store.start_agent_run("span-1", "trace-1", "sess-1", "gateway-remote", None).unwrap();
    for _ in 0..3 {
        store.record_event("span-1", &AgentEvent::Error { message: "Connection timeout".into() }).unwrap();
    }
    let issues = store.recent_issues(10).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].count, 3);
}

#[test]
fn memory_sources_discovers_existing_paths() {
    use cortex_lib::memory::sources::default_sources;
    let _ = default_sources(None, None);
}

#[test]
fn projects_discovery_runs_without_panic() {
    use cortex_lib::projects::discover_projects;
    let _ = cortex_lib::projects::discover_projects(None);
    let _ = discover_projects;
}
