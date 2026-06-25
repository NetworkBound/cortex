import { useEffect, useRef, useState } from "react";
import { getModels, postChat } from "../lib/api";
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

export default function ChatView() {
  const { activeProjectRoot } = useStore();
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState<string>("");
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);

  // Active run -> the assistant message we fold tokens into.
  const runToMsg = useRef<Map<string, string>>(new Map());
  const sessionId = useRef<string | undefined>(undefined);
  const { ref: scrollRef, notify } = useStickToBottom<HTMLDivElement>();

  useEffect(() => {
    getModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  useEffect(notify, [messages, notify]);

  const patch = (msgId: string, fn: (m: Message) => Message) =>
    setMessages((ms) => ms.map((m) => (m.id === msgId ? fn(m) : m)));

  useWs((f: WsFrameBase) => {
    const runId = f.run_id as string | undefined;
    if (!runId) return;
    const msgId = runToMsg.current.get(runId);
    if (!msgId) return; // not our run

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
        setSending(false);
        break;
      case "chat_error":
        patch(msgId, (m) => ({
          ...m,
          streaming: false,
          error: f.message as string,
        }));
        runToMsg.current.delete(runId);
        setSending(false);
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
      setSending(false);
    }
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
          <button className="send" onClick={send} disabled={!input.trim() || sending}>
            ↑
          </button>
        </div>
      </div>
    </>
  );
}

function MessageBubble({ m }: { m: Message }) {
  if (m.role === "user") {
    return <div className="msg user">{m.text}</div>;
  }
  return (
    <div className={`msg assistant${m.streaming && !m.text ? " streaming" : ""}`}>
      {m.reasoning && <div className="reasoning">{m.reasoning}</div>}
      {m.text ? (
        <Markdown>{m.text}</Markdown>
      ) : (
        !m.streaming && !m.error && <span className="faint">no output</span>
      )}
      {m.error && <div className="banner err" style={{ margin: "8px 0 0" }}>{m.error}</div>}
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
