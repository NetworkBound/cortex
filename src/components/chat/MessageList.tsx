import type { ReactNode, RefObject } from "react";
import type { Message, QueuedMessage } from "@/state/store";
import { MessageRow } from "./MessageRow";
import { latestFinishedRunMessageId } from "./run-summary";

/** A type-ahead submission parked while a turn streams (dimmed, cancellable). */
function QueuedMessageRow({
  q,
  onCancel,
}: {
  q: QueuedMessage;
  onCancel: (id: string) => void;
}) {
  return (
    <div className="msg msg-user msg-queued">
      <div className="msg-role">
        <strong>user</strong>
        <span className="msg-queued-badge">queued</span>
      </div>
      <div className="msg-content">
        <span className="md-prose">{q.content}</span>
        {q.images.length > 0 && (
          <span className="msg-queued-images">
            {q.images.length} image{q.images.length === 1 ? "" : "s"} attached
          </span>
        )}
      </div>
      <div className="msg-queued-foot">
        <span className="msg-queued-note">
          queued — sends when the current turn finishes
        </span>
        <button
          type="button"
          className="msg-queued-cancel"
          onClick={() => onCancel(q.id)}
          title="Remove from queue"
          aria-label="Cancel queued message"
        >
          ×
        </button>
      </div>
    </div>
  );
}

export interface MessageListProps {
  messages: Message[];
  queuedMessages: QueuedMessage[];
  setApproval: (id: string, a: null) => void;
  onRegenerate: (userContent: string) => void;
  onDequeue: (id: string) => void;
  /** Scroll container ref — ChatPane owns stick-to-bottom on it. */
  containerRef: RefObject<HTMLDivElement>;
  onScroll: () => void;
  /** Rendered instead of the list when there are no messages. */
  empty: ReactNode;
}

/**
 * The scrolling transcript: grouped message rows followed by any queued
 * type-ahead bubbles. Scroll behaviour stays in ChatPane (it reads the
 * container on every store change); this component only owns the markup.
 */
export function MessageList({
  messages,
  queuedMessages,
  setApproval,
  onRegenerate,
  onDequeue,
  containerRef,
  onScroll,
  empty,
}: MessageListProps) {
  // The one turn that gets the "Done — …" footer (FinishedRunCard).
  const runCardId = latestFinishedRunMessageId(messages);
  return (
    <div
      className="chat-messages"
      ref={containerRef}
      onScroll={onScroll}
      role="log"
      aria-live="polite"
      aria-label="Conversation"
    >
      {messages.length === 0 && empty}
      {messages.map((m, i) => {
        // Group consecutive turns from the same author (role + agent) so the
        // role label shows once per author run, not on every message.
        const prev = messages[i - 1];
        const continuesAuthor =
          !!prev && prev.role === m.role && prev.agent === m.agent;
        return (
          <MessageRow
            key={m.id}
            m={m}
            setApproval={setApproval}
            onRegenerate={onRegenerate}
            continuesAuthor={continuesAuthor}
            showRunCard={m.id === runCardId}
          />
        );
      })}
      {queuedMessages.map((q) => (
        <QueuedMessageRow key={q.id} q={q} onCancel={onDequeue} />
      ))}
    </div>
  );
}
