import { memo, useMemo, type ReactNode } from "react";
import { AlertTriangle, Paperclip } from "lucide-react";
import type { Message, ToolEvent } from "@/state/store";
import { extractPlan } from "@/lib/plan";
import { MarkdownView } from "../MarkdownView";
import { MessageActions } from "../MessageActions";
import { ReasoningBlock } from "../ReasoningBlock";
import { ToolCallCard } from "../ToolCallCard";
import { ApprovalPrompt } from "../ApprovalPrompt";
import { PlanCard } from "../PlanCard";
import { ThinkingIndicator } from "./ThinkingIndicator";
import { attachmentChips } from "./attachment-tokens";

// Render one run of assistant/user text: a PlanCard when it's a well-formed
// plan (Cline / Aider plan mode), otherwise markdown for the assistant and
// plain prose for the user. Shared by the flat fallback and the block timeline.
function renderTextContent(m: Message, text: string, key?: string): ReactNode {
  if (m.role === "assistant") {
    const plan = extractPlan(text);
    if (plan) {
      return (
        <div className="msg-content" key={key}>
          <PlanCard plan={plan} sessionId={m.id} />
        </div>
      );
    }
  }
  // Render markdown for every app-authored voice — the assistant AND the
  // system/error notes (`/test`, `/lint`, `/architect`, snapshot, repo-map…),
  // which are written WITH markdown. Only the user's own typed turn stays
  // verbatim (we don't reinterpret what they typed, and the attachment-chip
  // parsing relies on the raw content).
  const asMarkdown = m.role !== "user";
  return (
    <div className="msg-content" key={key}>
      {asMarkdown ? (
        <MarkdownView source={text} />
      ) : (
        <span className="md-prose">{text}</span>
      )}
    </div>
  );
}

// Render a message body. When an ordered block timeline is present (live or
// rehydrated turns), interleave text runs and tool cards in the order they
// streamed — narration sits above the tools it introduces, summaries below the
// tools they describe — matching Claude.ai / Cline / Cursor. Consecutive tool
// blocks are grouped into one card stack. Otherwise fall back to the flat
// "all tools, then all content" layout used by legacy messages.
function renderTimeline(m: Message): ReactNode {
  if (!m.blocks || m.blocks.length === 0) {
    return (
      <>
        {m.tools.length > 0 && (
          <div className="msg-tools">
            {m.tools.map((t) => (
              <ToolCallCard key={t.id} tool={t} />
            ))}
          </div>
        )}
        {m.content ? renderTextContent(m, m.content) : null}
      </>
    );
  }
  const out: ReactNode[] = [];
  for (let i = 0; i < m.blocks.length; ) {
    const b = m.blocks[i];
    if (b.type === "text") {
      if (b.text.trim()) out.push(renderTextContent(m, b.text, `t${i}`));
      i++;
    } else {
      const group: ToolEvent[] = [];
      while (i < m.blocks.length) {
        const bk = m.blocks[i];
        if (bk.type !== "tool") break;
        const tool = m.tools.find((t) => t.id === bk.toolId);
        if (tool) group.push(tool);
        i++;
      }
      if (group.length > 0) {
        out.push(
          <div className="msg-tools" key={`g${i}`}>
            {group.map((t) => (
              <ToolCallCard key={t.id} tool={t} />
            ))}
          </div>,
        );
      }
    }
  }
  return <>{out}</>;
}

export interface MessageRowProps {
  m: Message;
  setApproval: (id: string, a: null) => void;
  onRegenerate: (userContent: string) => void;
  // True when the previous message is from the same author (role + agent), so
  // we suppress the repeated role label and tighten the gap — the message
  // grouping every mature chat UI uses (Claude.ai / ChatGPT / Slack / Linear).
  continuesAuthor?: boolean;
}

// Memoized: with stable setApproval/onRegenerate props, only the message whose
// object actually changed re-renders. During streaming that's just the one
// in-flight bubble — prior bubbles no longer re-parse markdown per token.
export const MessageRow = memo(function MessageRow({
  m,
  setApproval,
  onRegenerate,
  continuesAuthor = false,
}: MessageRowProps) {
  // @-token chips for user messages so users can see at a glance what context
  // was attached on each turn. The persisted content is the ORIGINAL
  // (pre-expansion) typed message, so we re-parse the same patterns the
  // backend's `expand_at_tokens` recognises.
  const chips = useMemo(
    () => (m.role === "user" && m.content ? attachmentChips(m.content) : []),
    [m.content, m.role],
  );
  const isError = m.role === "error";
  return (
    <div
      className={`msg msg-${m.role}${continuesAuthor ? " msg-cont" : ""}`}
      role={isError ? "alert" : undefined}
    >
      {!continuesAuthor ? (
        <div className="msg-role">
          {isError && (
            <AlertTriangle size={12} strokeWidth={2} aria-hidden="true" />
          )}
          <strong>{m.agent ?? m.role}</strong>
          {m.pending && <span className="cursor"> ▎</span>}
        </div>
      ) : (
        // Grouped follow-up: the author label is suppressed, but keep the
        // streaming cursor visible while this turn is still generating.
        m.pending && <span className="cursor msg-cont-cursor">▎</span>
      )}
      {chips.length > 0 && (
        <div
          className="msg-attachments"
          role="list"
          aria-label="Brain attachments on this message"
        >
          {chips.map((c, i) => (
            <span
              key={i}
              className="msg-attachment-chip"
              role="listitem"
              data-mention={c.mention ? "true" : undefined}
              title={
                c.mention
                  ? `Auto-attached via implicit path mention: ${c.label}`
                  : undefined
              }
            >
              <Paperclip size={12} strokeWidth={1.75} aria-hidden="true" />{" "}
              {c.label}
            </span>
          ))}
        </div>
      )}
      {m.pending && !m.content && m.tools.length === 0 && !m.reasoning && (
        <ThinkingIndicator agent={m.agent} />
      )}
      {m.reasoning && (
        <ReasoningBlock reasoning={m.reasoning} messageId={m.id} />
      )}
      {renderTimeline(m)}
      {m.approval && (
        <ApprovalPrompt
          approval={m.approval}
          onResolved={() => setApproval(m.id, null)}
        />
      )}
      <MessageActions message={m} onRegenerate={onRegenerate} />
    </div>
  );
});
