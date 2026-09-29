//! Token-stream coalescing for the chat event loop.
//!
//! Streaming adapters (CLI subprocesses, SSE) deliver `AgentEvent::Token` /
//! `AgentEvent::Reasoning` one fragment at a time — often a few characters
//! each. `chat_send` used to forward every fragment as its own Tauri `emit`
//! (one IPC round-trip + JSON serialisation each) AND one SQLite `INSERT` into
//! the `events` table. A 2 000-token reply became ~4 000 IPC messages and
//! ~2 000 autocommit writes interleaved on a runtime worker, which is what
//! made the composer feel laggy mid-stream on Windows.
//!
//! [`CoalescingReceiver`] wraps the adapter's `mpsc::Receiver` and merges
//! *consecutive* text fragments of the same kind that arrive within a short
//! window (one display frame, ~16 ms). Ordering is preserved exactly: a
//! non-text event (tool call, error, done) is never reordered around text —
//! it simply ends the current batch and is returned on the next call. The
//! merged event is byte-identical to concatenating the fragments, so every
//! downstream consumer (frontend append, focus-chain scanner, trace store's
//! `chars` count) sees the same content with far fewer events.

use crate::agents::AgentEvent;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// One display frame at 60 Hz — the longest a fragment is held back.
pub const DEFAULT_WINDOW: Duration = Duration::from_millis(16);

/// Flush a batch once it reaches this many bytes even if the window is still
/// open, so a very fast producer can't build one huge event.
const MAX_BATCH_BYTES: usize = 8 * 1024;

pub struct CoalescingReceiver {
    rx: mpsc::Receiver<AgentEvent>,
    /// A non-mergeable event pulled while filling a batch; returned next.
    pending: Option<AgentEvent>,
    window: Duration,
}

impl CoalescingReceiver {
    pub fn new(rx: mpsc::Receiver<AgentEvent>) -> Self {
        Self::with_window(rx, DEFAULT_WINDOW)
    }

    pub fn with_window(rx: mpsc::Receiver<AgentEvent>, window: Duration) -> Self {
        Self {
            rx,
            pending: None,
            window,
        }
    }

    /// Next event, with consecutive same-kind text fragments merged. Returns
    /// `None` once the channel is closed and everything has been drained —
    /// the same contract as `mpsc::Receiver::recv`.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        let first = match self.pending.take() {
            Some(evt) => evt,
            None => self.rx.recv().await?,
        };
        if text_len(&first).is_none() {
            return Some(first);
        }
        let mut acc = first;
        let deadline = Instant::now() + self.window;
        loop {
            if text_len(&acc).unwrap_or(0) >= MAX_BATCH_BYTES {
                break;
            }
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(next)) => {
                    if !merge_text(&mut acc, &next) {
                        self.pending = Some(next);
                        break;
                    }
                }
                // Channel closed: hand back what we have; the next call
                // observes the closed channel and returns `None`.
                Ok(None) => break,
                // Window elapsed.
                Err(_) => break,
            }
        }
        Some(acc)
    }
}

/// Byte length of the text carried by a mergeable event, `None` otherwise.
fn text_len(evt: &AgentEvent) -> Option<usize> {
    match evt {
        AgentEvent::Token { delta } => Some(delta.len()),
        AgentEvent::Reasoning { text } => Some(text.len()),
        _ => None,
    }
}

/// Append `next` onto `acc` when both are text fragments of the same kind.
/// Returns `false` (leaving `acc` untouched) when they can't be merged.
pub fn merge_text(acc: &mut AgentEvent, next: &AgentEvent) -> bool {
    match (acc, next) {
        (AgentEvent::Token { delta }, AgentEvent::Token { delta: more }) => {
            delta.push_str(more);
            true
        }
        (AgentEvent::Reasoning { text }, AgentEvent::Reasoning { text: more }) => {
            text.push_str(more);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(s: &str) -> AgentEvent {
        AgentEvent::Token {
            delta: s.to_string(),
        }
    }

    #[test]
    fn merge_only_same_kind_text() {
        let mut a = tok("he");
        assert!(merge_text(&mut a, &tok("llo")));
        assert!(matches!(&a, AgentEvent::Token { delta } if delta == "hello"));

        // Token + Reasoning never merge, and the accumulator is untouched.
        let reasoning = AgentEvent::Reasoning {
            text: "thinking".into(),
        };
        assert!(!merge_text(&mut a, &reasoning));
        assert!(matches!(&a, AgentEvent::Token { delta } if delta == "hello"));

        // Non-text events are never mergeable.
        let mut done = AgentEvent::Done {
            total_tokens: None,
            run_id: None,
        };
        assert!(!merge_text(&mut done, &tok("x")));
        assert!(matches!(done, AgentEvent::Done { .. }));
    }

    #[tokio::test]
    async fn merges_burst_and_preserves_order_around_tool_call() {
        let (tx, rx) = mpsc::channel(64);
        for s in ["a", "b", "c"] {
            tx.send(tok(s)).await.unwrap();
        }
        tx.send(AgentEvent::ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            preview: None,
        })
        .await
        .unwrap();
        tx.send(tok("d")).await.unwrap();
        tx.send(tok("e")).await.unwrap();
        drop(tx);

        let mut rx = CoalescingReceiver::with_window(rx, Duration::from_millis(50));
        let first = rx.recv().await.unwrap();
        assert!(matches!(&first, AgentEvent::Token { delta } if delta == "abc"));
        let second = rx.recv().await.unwrap();
        assert!(matches!(second, AgentEvent::ToolCall { .. }));
        let third = rx.recv().await.unwrap();
        assert!(matches!(&third, AgentEvent::Token { delta } if delta == "de"));
        assert!(rx.recv().await.is_none());
        // Stays closed.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn flushes_when_window_elapses_before_next_fragment() {
        let (tx, rx) = mpsc::channel(64);
        tx.send(tok("slow")).await.unwrap();
        let mut rx = CoalescingReceiver::with_window(rx, Duration::from_millis(10));
        // Producer is silent — the window closes and the lone fragment is
        // delivered on its own rather than waiting for the channel to end.
        let evt = rx.recv().await.unwrap();
        assert!(matches!(&evt, AgentEvent::Token { delta } if delta == "slow"));
        tx.send(tok("later")).await.unwrap();
        drop(tx);
        let evt = rx.recv().await.unwrap();
        assert!(matches!(&evt, AgentEvent::Token { delta } if delta == "later"));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn caps_batch_size() {
        let (tx, rx) = mpsc::channel(256);
        let chunk = "x".repeat(1024);
        for _ in 0..12 {
            tx.send(tok(&chunk)).await.unwrap();
        }
        drop(tx);
        let mut rx = CoalescingReceiver::with_window(rx, Duration::from_millis(50));
        let first = rx.recv().await.unwrap();
        let AgentEvent::Token { delta } = first else {
            panic!("expected token")
        };
        assert_eq!(delta.len(), MAX_BATCH_BYTES);
        let mut rest = 0;
        while let Some(AgentEvent::Token { delta }) = rx.recv().await {
            rest += delta.len();
        }
        assert_eq!(rest, 4 * 1024);
    }
}
