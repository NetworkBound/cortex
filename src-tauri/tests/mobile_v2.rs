//! Serialisation goldens for the mobile v2 wire shapes (WebSocket events and
//! REST response types) + the pure auth-gate matrix. These pin the JSON the
//! mobile client (`mobile/src/lib/types.ts`) is written against — a change
//! here is a contract change.

use cortex_lib::mobile_server::events::{
    ApprovalView, MessageView, RunView, ThreadView, ToolCallView, UsageView, V2Event,
};
use cortex_lib::mobile_server::pairing::{bearer_token, check_access, is_local_peer, Denied};
use serde_json::{json, Value};
use std::net::IpAddr;

fn tool() -> ToolCallView {
    ToolCallView {
        id: "tc-1-01hxyz".into(),
        name: "bash".into(),
        args_preview: "ls -la".into(),
        status: "done".into(),
        result_preview: Some("3 files".into()),
        duration_ms: Some(12),
    }
}

fn approval() -> ApprovalView {
    ApprovalView {
        id: "gw-run-1".into(),
        tool: "bash".into(),
        detail: "rm -rf build".into(),
        resolved: false,
        args_preview: "rm -rf build".into(),
        risk: "exec".into(),
        thread_id: "session-1".into(),
        run_id: "run-1".into(),
        choices: vec!["once".into(), "deny".into()],
        created_ms: 1_700_000_000_000,
        ts_ms: 1_700_000_000_000,
    }
}

fn thread() -> ThreadView {
    ThreadView {
        id: "session-1".into(),
        title: "Fix the build".into(),
        project_root: Some("/home/me/proj".into()),
        agent_id: Some("claude-cli".into()),
        model: None,
        created_ms: 1,
        last_ms: 2,
        pending_approvals: 1,
        running: true,
        last_preview: "Working on it".into(),
    }
}

fn v(e: &V2Event) -> Value {
    serde_json::to_value(e).unwrap()
}

#[test]
fn ws_event_goldens() {
    assert_eq!(
        v(&V2Event::Token {
            thread_id: "t".into(),
            run_id: "r".into(),
            message_id: "m".into(),
            delta: "hi".into(),
        }),
        json!({ "type": "token", "thread_id": "t", "run_id": "r", "message_id": "m", "delta": "hi" })
    );
    assert_eq!(
        v(&V2Event::Reasoning {
            thread_id: "t".into(),
            run_id: "r".into(),
            message_id: "m".into(),
            delta: "think".into(),
        }),
        json!({ "type": "reasoning", "thread_id": "t", "run_id": "r", "message_id": "m", "delta": "think" })
    );
    assert_eq!(
        v(&V2Event::ToolCall {
            thread_id: "t".into(),
            run_id: "r".into(),
            message_id: "m".into(),
            tool: tool(),
        }),
        json!({
            "type": "tool_call", "thread_id": "t", "run_id": "r", "message_id": "m",
            "tool": { "id": "tc-1-01hxyz", "name": "bash", "args_preview": "ls -la",
                      "status": "done", "result_preview": "3 files", "duration_ms": 12 }
        })
    );
    assert_eq!(
        v(&V2Event::ToolResult {
            thread_id: "t".into(),
            run_id: "r".into(),
            message_id: "m".into(),
            tool: ToolCallView {
                result_preview: None,
                duration_ms: None,
                ..tool()
            },
        })["tool"],
        json!({ "id": "tc-1-01hxyz", "name": "bash", "args_preview": "ls -la", "status": "done" })
    );
    assert_eq!(
        v(&V2Event::ApprovalRequest {
            thread_id: "session-1".into(),
            run_id: "run-1".into(),
            approval: approval(),
        }),
        json!({
            "type": "approval_request", "thread_id": "session-1", "run_id": "run-1",
            "approval": {
                "id": "gw-run-1", "tool": "bash", "detail": "rm -rf build", "resolved": false,
                "args_preview": "rm -rf build", "risk": "exec", "thread_id": "session-1",
                "run_id": "run-1", "choices": ["once", "deny"],
                "created_ms": 1_700_000_000_000i64, "ts_ms": 1_700_000_000_000i64
            }
        })
    );
    assert_eq!(
        v(&V2Event::ApprovalResolved {
            approval_id: "gw-run-1".into(),
            decision: "approve".into(),
            thread_id: "session-1".into(),
            run_id: "run-1".into(),
        }),
        json!({ "type": "approval_resolved", "approval_id": "gw-run-1", "decision": "approve",
                "thread_id": "session-1", "run_id": "run-1" })
    );
    assert_eq!(
        v(&V2Event::Done {
            thread_id: "t".into(),
            run_id: "r".into(),
            usage: Some(UsageView {
                input_tokens: 50,
                output_tokens: 50,
                cost_usd: Some(0.00025),
            }),
        }),
        json!({ "type": "done", "thread_id": "t", "run_id": "r",
                "usage": { "input_tokens": 50, "output_tokens": 50, "cost_usd": 0.00025 } })
    );
    assert_eq!(
        v(&V2Event::Done {
            thread_id: "t".into(),
            run_id: "r".into(),
            usage: None,
        }),
        json!({ "type": "done", "thread_id": "t", "run_id": "r" })
    );
    assert_eq!(
        v(&V2Event::Error {
            thread_id: "t".into(),
            run_id: "r".into(),
            message: "boom".into(),
        }),
        json!({ "type": "error", "thread_id": "t", "run_id": "r", "message": "boom" })
    );
    assert_eq!(
        v(&V2Event::ThreadUpdated { thread: thread() }),
        json!({ "type": "thread_updated", "thread": {
            "id": "session-1", "title": "Fix the build", "project_root": "/home/me/proj",
            "agent_id": "claude-cli", "model": null, "created_ms": 1, "last_ms": 2,
            "pending_approvals": 1, "running": true, "last_preview": "Working on it" } })
    );
    assert_eq!(v(&V2Event::Ping), json!({ "type": "ping" }));
}

#[test]
fn ws_events_round_trip_through_deserialize() {
    for e in [
        V2Event::Ping,
        V2Event::ThreadUpdated { thread: thread() },
        V2Event::ApprovalRequest {
            thread_id: "session-1".into(),
            run_id: "run-1".into(),
            approval: approval(),
        },
    ] {
        let back: V2Event = serde_json::from_value(v(&e)).unwrap();
        assert_eq!(back, e);
    }
}

#[test]
fn message_and_run_goldens() {
    let m = MessageView {
        id: "asst-1".into(),
        role: "assistant".into(),
        content: "Done.".into(),
        ts_ms: 5,
        run_id: Some("run-1".into()),
        tool_calls: Some(vec![tool()]),
        approval: None,
        error: None,
        routing_reason: Some("explicit pick".into()),
        reasoning: None,
        agent_id: Some("claude-cli".into()),
        pending: false,
    };
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        json!({
            "id": "asst-1", "role": "assistant", "content": "Done.", "ts_ms": 5, "run_id": "run-1",
            "tool_calls": [{ "id": "tc-1-01hxyz", "name": "bash", "args_preview": "ls -la",
                             "status": "done", "result_preview": "3 files", "duration_ms": 12 }],
            "routing_reason": "explicit pick", "agent_id": "claude-cli"
        })
    );
    // A pending (in-flight) message carries `pending: true`; a plain user
    // message carries none of the optional keys.
    let u = MessageView {
        id: "user-1".into(),
        role: "user".into(),
        content: "hi".into(),
        ts_ms: 1,
        run_id: None,
        tool_calls: None,
        approval: None,
        error: None,
        routing_reason: None,
        reasoning: None,
        agent_id: None,
        pending: true,
    };
    assert_eq!(
        serde_json::to_value(&u).unwrap(),
        json!({ "id": "user-1", "role": "user", "content": "hi", "ts_ms": 1, "pending": true })
    );
    let r = RunView {
        run_id: "span-1".into(),
        thread_id: "session-1".into(),
        started_ms: 10,
        ended_ms: Some(20),
        status: "done".into(),
        agent_id: Some("claude-cli".into()),
        model: Some("claude-sonnet-4-6".into()),
        cost_usd: Some(0.01),
        tokens: Some(1000),
    };
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({ "run_id": "span-1", "thread_id": "session-1", "started_ms": 10, "ended_ms": 20,
                "status": "done", "agent_id": "claude-cli", "model": "claude-sonnet-4-6",
                "cost_usd": 0.01, "tokens": 1000 })
    );
}

#[test]
fn auth_gate_matrix_pure_parts() {
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    let lan: IpAddr = "192.168.0.2".parse().unwrap();
    assert!(is_local_peer(Some(lo), false, false));
    assert!(!is_local_peer(Some(lo), true, false)); // forwarded (tailscale serve)
    assert!(!is_local_peer(Some(lo), false, true)); // CORTEX_E2E_FORCE_AUTH
    assert!(!is_local_peer(Some(lan), false, false));
    assert!(!is_local_peer(None, false, false));
    assert_eq!(bearer_token("Bearer x"), Some("x"));
    assert_eq!(bearer_token("Token x"), None);
    // Missing vs invalid are distinct 401s (the registry read for an unknown
    // token is a file miss in the test home — still `Invalid`).
    assert_eq!(
        check_access(Some(lan), false, false, None, None),
        Err(Denied::Missing)
    );
    assert_eq!(
        check_access(
            Some(lan),
            false,
            false,
            Some("Bearer definitely-not-paired"),
            None
        ),
        Err(Denied::Invalid)
    );
    assert_eq!(
        Denied::Missing.message(),
        "missing bearer token: pair this device first"
    );
}
