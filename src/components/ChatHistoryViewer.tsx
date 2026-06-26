import { useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createPortal } from "react-dom";
import {
  getClaudeChat,
  type ChatSummary,
  type ChatTranscript,
  type ChatTurn,
} from "@/lib/chat-history";
import { importToBrain } from "@/lib/brain-import";

/**
 * Modal viewer for a full Claude chat transcript. Opened from
 * ChatHistorySidebar's per-row "View" action. Loads up to 500 turns via
 * `getClaudeChat`, then renders role-colored message bubbles with a
 * client-side search filter and two side-effects:
 *   • "Copy as markdown" → clipboard
 *   • "Save to brain"    → `importToBrain` (Cortex Brain vault)
 *
 * Uses a portal so the backdrop overlays the entire app layout, matching
 * the convention used by SettingsModal / ChangelogModal.
 */

const MAX_TURNS = 500;

interface Props {
  chat: ChatSummary;
  onClose: () => void;
}

function renderRoleLabel(role: string): string {
  if (role === "user") return "you";
  if (role === "assistant") return "claude";
  return role;
}

function transcriptToMarkdown(t: ChatTranscript, chat: ChatSummary): string {
  const header = [
    `# Chat — ${chat.project ?? "(global)"}`,
    "",
    `- session: \`${chat.session_id}\``,
    `- file: \`${chat.file_path}\``,
    `- turns: ${t.turns.length}`,
    "",
    "---",
    "",
  ].join("\n");
  const body = t.turns
    .map((turn) => `**${renderRoleLabel(turn.role)}:**\n\n${turn.content}\n`)
    .join("\n---\n\n");
  return header + body;
}

export function ChatHistoryViewer({ chat, onClose }: Props) {
  const [transcript, setTranscript] = useState<ChatTranscript | null>(null);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    getClaudeChat(chat.file_path, MAX_TURNS)
      .then((t) => {
        if (!alive) return;
        setTranscript(t);
        setErr(null);
      })
      .catch((e) => {
        if (alive) setErr(humanizeError(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [chat.file_path]);

  // Escape closes; mirrors the convention from other modals.
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const turns: ChatTurn[] = transcript?.turns ?? [];

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return turns;
    return turns.filter((t) => t.content.toLowerCase().includes(q));
  }, [turns, query]);

  const copyMarkdown = async () => {
    if (!transcript) return;
    try {
      const md = transcriptToMarkdown(transcript, chat);
      await navigator.clipboard.writeText(md);
      setStatus(`Copied ${md.length} chars to clipboard.`);
    } catch (e) {
      setStatus(`Copy failed: ${humanizeError(e)}`);
    }
  };

  const saveToBrain = async () => {
    if (!transcript) return;
    try {
      const md = transcriptToMarkdown(transcript, chat);
      const label = `${chat.project ?? "global"}-${chat.session_id.slice(-8)}`;
      const res = await importToBrain(md, label, "chat");
      setStatus(`Saved → ${res.written_path}`);
    } catch (e) {
      setStatus(`Save failed: ${humanizeError(e)}`);
    }
  };

  const title =
    chat.first_message?.slice(0, 80) ||
    `session ${chat.session_id.slice(-10)}`;

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal chat-history-viewer"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="chat-history-viewer-head">
          <div className="chat-history-viewer-titles">
            <h2>{title}</h2>
            <div className="muted chat-history-viewer-sub">
              {chat.project ?? "(global)"} · {turns.length} of{" "}
              {chat.message_count} msgs · {chat.session_id.slice(-8)}
            </div>
          </div>
          <button className="link-btn" onClick={onClose} aria-label="Close">
            Close
          </button>
        </div>

        <div className="chat-history-viewer-toolbar">
          <input
            className="chat-history-viewer-search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search this transcript…"
          />
          <button
            className="link-btn"
            disabled={!transcript}
            onClick={() => void copyMarkdown()}
          >
            Copy as markdown
          </button>
          <button
            className="link-btn"
            disabled={!transcript}
            onClick={() => void saveToBrain()}
          >
            Save to brain
          </button>
        </div>

        {status && <div className="chat-history-viewer-status">{status}</div>}
        {err && <div className="chat-history-error">{err}</div>}
        {loading && <div className="chat-history-empty">loading transcript…</div>}

        {!loading && !err && (
          <div className="chat-history-viewer-body">
            {filtered.length === 0 && (
              <div className="chat-history-empty">
                {query
                  ? `No turns match "${query}".`
                  : "Transcript is empty."}
              </div>
            )}
            {filtered.map((t, i) => (
              <div
                key={`${t.ts_unix_ms ?? "x"}-${i}`}
                className={`chat-history-bubble chat-history-bubble-${t.role}`}
              >
                <div className="chat-history-bubble-role">
                  {renderRoleLabel(t.role)}
                </div>
                <div className="chat-history-bubble-content">{t.content}</div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
