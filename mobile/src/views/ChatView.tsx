import { useEffect, useRef, useState } from "react";
import { getModels, getSessionMessages, postChat } from "../lib/api";
import { useWs } from "../lib/useWs";
import { useStore } from "../lib/store";
import { useStickToBottom } from "../lib/scroll";
import Markdown from "../components/Markdown";
import type { WsFrameBase } from "../lib/types";

interface ToolEvent {
  name: string;
  ok?: boolean;
  summary?: string;
  preview?: string;
}

interface Message {
  id: string;
  role: "user" | "assistant";
  text: string;
  reasoning?: string;
  tools: ToolEvent[];
  streaming?: boolean;
  error?: string;
}

let mid = 0;
const nextId = () => `m${++mid}`;

// If a streaming reply produces no WS frame for this long, assume the run is
// wedged (e.g. the socket dropped mid-stream) and free the composer.
const SEND_WATCHDOG_MS = 45_000;

export default function ChatView() {
  const { activeProjectRoot, openSession, setOpenSession, wsStatus, newChatNonce } =
    useStore();
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState<string>("");
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);

  // Active run -> the assistant message we fold tokens into.
  const runToMsg = useRef<Map<string, string>>(new Map());
  const sessionId = useRef<string | undefined>(undefined);
  // The assistant message of the in-flight send, so the watchdog can flag it.
  const pendingMsg = useRef<string | null>(null);
  const watchdog = useRef<ReturnType<typeof setTimeout> | null>(null);
  const { ref: scrollRef, notify } = useStickToBottom<HTMLDivElement>();

  useEffect(() => {
    getModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  useEffect(notify, [messages, notify]);

  // Resume a session the Recent tab handed off: load its stored history into
  // the conversation and adopt its session_id so the composer continues it.
  // System rows (auto-injected project context) are kept in the session but not
  // rendered as bubbles. Clears `openSession` once consumed.
  useEffect(() => {
    if (!openSession) return;
    const id = openSession;
    setOpenSession(null);
    sessionId.current = id;
    runToMsg.current.clear();
    setSending(false);
    getSessionMessages(id)
      .then((stored) => {
        const loaded: Message[] = stored
          .filter((s) => s.role === "user" || s.role === "assistant")
          .map((s) => ({
            id: nextId(),
            role: s.role === "user" ? "user" : "assistant",
            text: s.content,
            reasoning: s.reasoning ?? undefined,
            tools: [],
          }));
        setMessages(loaded);
      })
      .catch((e) => {
        setMessages([
          {
            id: nextId(),
            role: "assistant",
            text: "",
            tools: [],
            error: `Failed to load chat: ${e instanceof Error ? e.message : String(e)}`,
          },
        ]);
      });
  }, [openSession, setOpenSession]);

  const patch = (msgId: string, fn: (m: Message) => Message) =>
    setMessages((ms) => ms.map((m) => (m.id === msgId ? fn(m) : m)));

  const clearWatchdog = () => {
    if (watchdog.current) {
      clearTimeout(watchdog.current);
      watchdog.current = null;
    }
  };

  // Mark the current send as finished: stop the watchdog and unlock the
  // composer. Safe to call from the watchdog, WS terminal frames, or Stop.
  const finishSend = () => {
    clearWatchdog();
    pendingMsg.current = null;
    setSending(false);
  };

  // (Re)arm the watchdog. Every WS frame for the active run resets it; if it
  // ever fires, the run is presumed dead and we surface an error rather than
  // leaving the composer locked forever.
  const armWatchdog = () => {
    clearWatchdog();
    watchdog.current = setTimeout(() => {
      const msgId = pendingMsg.current;
      if (msgId) {
        patch(msgId, (m) =>
          m.streaming || !m.text
            ? {
                ...m,
                streaming: false,
                error:
                  "Connection lost — the reply stopped streaming. Try sending again.",
              }
            : { ...m, streaming: false },
        );
      }
      finishSend();
    }, SEND_WATCHDOG_MS);
  };

  // New-chat affordance (header button): drop history + detach the session so
  // the next message starts fresh.
  useEffect(() => {
    if (newChatNonce === 0) return;
    finishSend();
    runToMsg.current.clear();
    sessionId.current = undefined;
    setMessages([]);
    setInput("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newChatNonce]);

  // Tidy the timer if the view unmounts mid-send.
  useEffect(() => clearWatchdog, []);

  useWs((f: WsFrameBase) => {
    const runId = f.run_id as string | undefined;
    if (!runId) return;
    const msgId = runToMsg.current.get(runId);
    if (!msgId) return; // not our run

    // Liveness: any frame for our run means the stream is alive — keep the
    // watchdog at bay.
    armWatchdog();

    switch (f.type) {
      case "chat_token":
        patch(msgId, (m) => ({ ...m, text: m.text + (f.delta as string) }));
        break;
      case "chat_reasoning":
        patch(msgId, (m) => ({
          ...m,
          reasoning: (m.reasoning ?? "") + (f.text as string),
        }));
        break;
      case "chat_tool_call":
        patch(msgId, (m) => ({
          ...m,
          tools: [
            ...m.tools,
            { name: f.name as string, preview: f.preview as string | undefined },
          ],
        }));
        break;
      case "chat_tool_result":
        patch(msgId, (m) => {
          // Attach result to the last matching call, else append.
          const tools = [...m.tools];
          for (let i = tools.length - 1; i >= 0; i--) {
            if (tools[i].name === f.name && tools[i].ok === undefined) {
              tools[i] = {
                ...tools[i],
                ok: f.ok as boolean,
                summary: f.summary as string,
              };
              return { ...m, tools };
            }
          }
          tools.push({
            name: f.name as string,
            ok: f.ok as boolean,
            summary: f.summary as string,
          });
          return { ...m, tools };
        });
        break;
      case "chat_file_edit":
        patch(msgId, (m) => ({
          ...m,
          tools: [
            ...m.tools,
            {
              name: "edit",
              ok: true,
              summary: `${f.path} (${f.lines_changed} lines)`,
            },
          ],
        }));
        break;
      case "chat_done":
        patch(msgId, (m) => ({ ...m, streaming: false }));
        runToMsg.current.delete(runId);
        finishSend();
        break;
      case "chat_error":
        patch(msgId, (m) => ({
          ...m,
          streaming: false,
          error: f.message as string,
        }));
        runToMsg.current.delete(runId);
        finishSend();
        break;
    }
  });

  const send = async () => {
    const text = input.trim();
    if (!text || sending) return;
    setInput("");
    setSending(true);

    const userMsg: Message = { id: nextId(), role: "user", text, tools: [] };
    const asstMsg: Message = {
      id: nextId(),
      role: "assistant",
      text: "",
      tools: [],
      streaming: true,
    };
    setMessages((ms) => [...ms, userMsg, asstMsg]);
    pendingMsg.current = asstMsg.id;
    // Guard the whole round-trip (POST + stream) so a drop can't lock us up.
    armWatchdog();

    try {
      const res = await postChat({
        session_id: sessionId.current,
        message: text,
        model: model || undefined,
        project_root: activeProjectRoot,
      });
      sessionId.current = res.session_id;
      runToMsg.current.set(res.run_id, asstMsg.id);
    } catch (e) {
      patch(asstMsg.id, (m) => ({
        ...m,
        streaming: false,
        error: e instanceof Error ? e.message : String(e),
      }));
      finishSend();
    }
  };

  // Stop waiting on the current reply. The backend keeps running, but we free
  // the composer and mark the bubble so the user isn't stuck.
  const stop = () => {
    const msgId = pendingMsg.current;
    if (msgId) {
      patch(msgId, (m) => ({ ...m, streaming: false }));
      const runId = [...runToMsg.current.entries()].find(
        ([, v]) => v === msgId,
      )?.[0];
      if (runId) runToMsg.current.delete(runId);
    }
    finishSend();
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <>
      <div className="scroll" ref={scrollRef}>
        {messages.length === 0 ? (
          <div className="empty">
            Ask Cortex anything. Replies stream live.
            <br />
            {activeProjectRoot ? "" : "Pick a project to give it repo context."}
          </div>
        ) : (
          <div className="msg-list">
            {messages.map((m) => (
              <MessageBubble key={m.id} m={m} />
            ))}
          </div>
        )}
      </div>

      <div className="composer">
        {sending && wsStatus !== "open" && (
          <div className="banner reconnecting" role="status" aria-live="polite">
            <span className="spin" aria-hidden="true" />
            Reconnecting… your reply will resume when the link is back.
          </div>
        )}
        {models.length > 0 && (
          <div className="model-bar">
            <select
              className="model-select"
              value={model}
              onChange={(e) => setModel(e.target.value)}
            >
              <option value="">Default (gateway)</option>
              {models.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </div>
        )}
        <div className="row">
          <textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={onKey}
            placeholder="Message Cortex…"
            rows={1}
          />
          {sending ? (
            <button
              className="send stop"
              onClick={stop}
              aria-label="Stop generating"
              title="Stop"
            >
              ◼
            </button>
          ) : (
            <button
              className="send"
              onClick={send}
              disabled={!input.trim()}
              aria-label="Send message"
              title="Send"
            >
              ↑
            </button>
          )}
        </div>
      </div>
    </>
  );
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard may be unavailable (insecure origin) — fail quietly */
    }
  };
  return (
    <button
      className="msg-copy"
      onClick={copy}
      aria-label={copied ? "Copied" : "Copy message"}
      title={copied ? "Copied" : "Copy"}
    >
      {copied ? "✓ Copied" : "⧉ Copy"}
    </button>
  );
}

function MessageBubble({ m }: { m: Message }) {
  if (m.role === "user") {
    return <div className="msg user">{m.text}</div>;
  }
  return (
    <div className={`msg assistant${m.streaming && !m.text ? " streaming" : ""}`}>
      {m.reasoning && (
        <details className="reasoning-toggle">
          <summary>Reasoning</summary>
          <div className="reasoning">{m.reasoning}</div>
        </details>
      )}
      {m.text ? (
        <Markdown>{m.text}</Markdown>
      ) : (
        !m.streaming && !m.error && <span className="faint">no output</span>
      )}
      {m.error && <div className="banner err" style={{ margin: "8px 0 0" }}>{m.error}</div>}
      {!m.streaming && m.text && (
        <div className="msg-actions">
          <CopyButton text={m.text} />
        </div>
      )}
      {m.tools.length > 0 && (
        <div className="tool-strip">
          {m.tools.map((t, i) => (
            <span
              key={i}
              className={`tool-chip ${t.ok === undefined ? "" : t.ok ? "ok" : "fail"}`}
              title={t.summary || t.preview || ""}
            >
              🔧 {t.name}
              {t.ok === false && " ✗"}
            </span>
          ))}
        </div>
      )}
      {m.text && m.streaming && <span className="faint"> </span>}
    </div>
  );
}
