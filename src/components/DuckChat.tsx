import { useCallback, useEffect, useRef, useState } from "react";
import { CloudOff, Settings } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  duckQuestion,
  saveDuckTranscript,
  type DuckTurn,
} from "@/lib/duck";
import { useGatewayConfigured } from "@/lib/gateway";
import { useCortexStore } from "@/state/store";
import { pushToast } from "@/lib/toast";

/**
 * Rubber-duck Socratic chat modal. Renders as a self-mounting portal so the
 * `/duck` slash command can summon it without touching App.tsx — same
 * pattern as `ExplainModal` / `DocGenModal`.
 *
 * On mount we kick off the first duck question for the given topic. The user
 * replies in the bottom input; each submit appends a user turn + asks the
 * backend for the next duck turn. Transcript lives entirely in component
 * state — the backend is stateless and replays it on each call.
 *
 * Actions: Reset (clears the transcript and re-asks the opening question),
 * Save transcript to brain (writes a markdown file under
 * `~/Documents/Cortex Brain/duck/`).
 */

interface DuckChatProps {
  initialTopic: string;
  onClose: () => void;
}

export function DuckChat({ initialTopic, onClose }: DuckChatProps) {
  const [topic] = useState<string>(initialTopic.trim() || "(no topic)");
  const [transcript, setTranscript] = useState<DuckTurn[]>([]);
  const [draft, setDraft] = useState<string>("");
  const [busy, setBusy] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState<boolean>(false);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  // Bump to reset — keeps the open-question effect re-firing without us
  // tracking it through transcript-length heuristics.
  const [resetTick, setResetTick] = useState(0);
  // The duck's questions are synthesized by an LLM served through the gateway
  // gateway. A standalone build (no gateway) can't produce them, so instead of
  // firing a doomed `duck_question` that returns a raw error, we degrade to a
  // humanized notice. `null` = still checking, `false` = standalone.
  const gateway = useGatewayConfigured();
  const gatewayMissing = gateway === false;

  // Kick off the opening question whenever we mount or the user hits Reset —
  // but only once a gateway is confirmed (re-fires if one is connected while
  // the modal is open, via the gateway-config-changed signal).
  useEffect(() => {
    if (gateway !== true) return;
    let cancelled = false;
    (async () => {
      setBusy(true);
      setError(null);
      try {
        const next = await duckQuestion(topic, []);
        if (cancelled) return;
        setTranscript([next]);
      } catch (e) {
        if (cancelled) return;
        setError(humanizeError(e));
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [topic, resetTick, gateway]);

  // ESC closes — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Pin the transcript to the bottom on update.
  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [transcript, busy]);

  const sendMessage = useCallback(async () => {
    const trimmed = draft.trim();
    if (!trimmed || busy || gatewayMissing) return;
    const userTurn: DuckTurn = {
      role: "user",
      content: trimmed,
      ts_unix_ms: Date.now(),
    };
    // Optimistically append the user bubble so the UI updates instantly.
    const nextTranscript = [...transcript, userTurn];
    setTranscript(nextTranscript);
    setDraft("");
    setBusy(true);
    setError(null);
    try {
      const reply = await duckQuestion(topic, nextTranscript);
      setTranscript((prev) => [...prev, reply]);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [draft, busy, transcript, topic, gatewayMissing]);

  const onReset = useCallback(() => {
    if (busy) return;
    setTranscript([]);
    setDraft("");
    setError(null);
    setResetTick((t) => t + 1);
  }, [busy]);

  const onSave = useCallback(async () => {
    if (saving || transcript.length === 0) return;
    setSaving(true);
    try {
      const saved = await saveDuckTranscript(topic, transcript);
      pushToast({
        title: "Saved to Brain",
        body: saved.written_path,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setSaving(false);
    }
  }, [saving, transcript, topic]);

  const onKeyDownInput = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      // Enter sends, Shift+Enter inserts a newline.
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void sendMessage();
      }
    },
    [sendMessage],
  );

  return (
    <div className="duck-backdrop" onClick={onClose}>
      <div
        className="duck-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="duck-title"
      >
        <header className="duck-header">
          <div className="duck-header-main">
            <h2 id="duck-title">Rubber duck</h2>
            <span className="duck-topic-chip" title={topic}>
              {topic}
            </span>
          </div>
          <div className="duck-header-actions">
            <button
              className="duck-action"
              onClick={onReset}
              disabled={busy || gatewayMissing}
              title="Clear transcript and re-ask the opening question"
            >
              Reset
            </button>
            <button
              className="duck-close"
              onClick={onClose}
              aria-label="Close"
            >
              ×
            </button>
          </div>
        </header>

        <div className="duck-transcript" ref={scrollerRef}>
          {gatewayMissing && (
            <div className="duck-gateway-notice" role="status">
              <CloudOff size={16} strokeWidth={1.9} aria-hidden="true" />
              <div className="duck-gateway-copy">
                <strong>Rubber duck needs a gateway</strong>
                <span>
                  The duck's questions are generated by an LLM served through
                  the Cortex Gateway. Connect one in Settings → Connection to
                  start a session.
                </span>
              </div>
              <button
                className="duck-gateway-btn"
                onClick={() => {
                  onClose();
                  useCortexStore.getState().setShowSettings(true);
                }}
              >
                <Settings size={13} strokeWidth={1.9} aria-hidden="true" />
                Open Settings
              </button>
            </div>
          )}
          {transcript.length === 0 && busy && (
            <div className="duck-loading">
              <span className="duck-spinner" aria-hidden /> waking the duck…
            </div>
          )}
          {transcript.map((turn, idx) => (
            <div
              key={`${turn.ts_unix_ms}-${idx}`}
              className={`duck-bubble duck-bubble-${turn.role}`}
            >
              <div className="duck-bubble-role">
                {turn.role === "duck" ? "Duck" : "You"}
              </div>
              <div className="duck-bubble-body">{turn.content}</div>
            </div>
          ))}
          {busy && transcript.length > 0 && (
            <div className="duck-bubble duck-bubble-duck duck-bubble-pending">
              <div className="duck-bubble-role">Duck</div>
              <div className="duck-bubble-body">
                <span className="duck-spinner" aria-hidden /> thinking…
              </div>
            </div>
          )}
          {error && (
            <div className="duck-error" role="alert">
              {error}
            </div>
          )}
        </div>

        <footer className="duck-footer">
          <textarea
            className="duck-input"
            placeholder={
              gatewayMissing
                ? "Connect a gateway to rubber-duck…"
                : busy
                  ? "duck is asking…"
                  : "Type your thinking, press Enter to send"
            }
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onKeyDownInput}
            rows={2}
            disabled={gatewayMissing || (busy && transcript.length === 0)}
            aria-label="Reply"
          />
          <div className="duck-footer-actions">
            <button
              className="duck-action"
              onClick={onSave}
              disabled={saving || transcript.length === 0}
              title="Write the transcript into ~/Documents/Cortex Brain/duck/"
            >
              {saving ? "Saving…" : "Save transcript to brain"}
            </button>
            <button
              className="duck-primary"
              onClick={() => void sendMessage()}
              disabled={busy || !draft.trim() || gatewayMissing}
            >
              Send
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/duck` slash command. Same detached-root
 * pattern as `ExplainModal` / `DocGenModal`.
 */
let activeRoot: Root | null = null;

export function openDuckChat(topic: string): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "duck";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<DuckChat initialTopic={topic} onClose={close} />);
}
