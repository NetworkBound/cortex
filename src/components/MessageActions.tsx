import "@/styles/message-actions.css";
import { humanizeError } from "@/lib/errors";
import { useCortexStore, type Message } from "@/state/store";
import { pushToast } from "@/lib/toast";
import { recordMessage } from "@/lib/sessions";
import { createMemoryEntry } from "@/lib/memory";
import { promptDialog } from "@/lib/dialogs";
import { playSound } from "@/lib/sounds";

export interface MessageActionsProps {
  message: Message;
  /**
   * Re-send a prior user message through the existing chat path. ChatPane
   * owns this so we don't reimplement streaming or routing.
   */
  onRegenerate: (userContent: string) => void;
}

/**
 * Per-message hover actions: copy, regenerate (assistant only), branch into
 * a new session, and pin to memory. Rendered inside each `.msg` element;
 * visibility is driven entirely by CSS (`.msg:hover .message-actions`).
 */
export function MessageActions({ message, onRegenerate }: MessageActionsProps) {
  const messages = useCortexStore((s) => s.messages);
  const activeProject = useCortexStore((s) => s.activeProject);
  const sessionId = useCortexStore((s) => s.sessionId);
  const resumeSession = useCortexStore((s) => s.resumeSession);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(message.content);
      pushToast({ title: "copied", kind: "success", ttlMs: 1800 });
      playSound("tick");
    } catch (e) {
      pushToast({ title: "copy failed", body: humanizeError(e), kind: "error" });
    }
  };

  const handleRegenerate = () => {
    if (message.role !== "assistant") return;
    const idx = messages.findIndex((m) => m.id === message.id);
    if (idx < 0) return;
    // Walk back to the nearest user message preceding this assistant turn.
    let userIdx = -1;
    for (let i = idx - 1; i >= 0; i--) {
      if (messages[i].role === "user") { userIdx = i; break; }
    }
    if (userIdx < 0) {
      pushToast({ title: "nothing to regenerate", body: "no prior user message", kind: "warning" });
      return;
    }
    const userMsg = messages[userIdx];
    // Defer to ChatPane: it owns the send pipeline and will reset state.
    onRegenerate(userMsg.content);
  };

  const handleBranch = async () => {
    const idx = messages.findIndex((m) => m.id === message.id);
    if (idx < 0) return;
    const slice = messages.slice(0, idx + 1).map((m) => ({ ...m, pending: false, approval: null }));
    const newId = `session-${crypto.randomUUID()}`;
    try {
      for (const m of slice) {
        // Skip transient error rows — they're not part of the conversation history.
        if (m.role === "error") continue;
        await recordMessage({
          id: m.id,
          sessionId: newId,
          role: m.role,
          agentId: m.agent ?? null,
          content: m.content,
          runId: m.runId ?? null,
          reasoning: m.reasoning ?? null,
          projectRoot: activeProject?.root ?? null,
        });
      }
      resumeSession(newId, slice);
      pushToast({ title: "branched", body: `${slice.length} messages copied`, kind: "success" });
    } catch (e) {
      pushToast({ title: "branch failed", body: humanizeError(e), kind: "error" });
    }
  };

  const handlePin = async () => {
    const name = await promptDialog({ title: "Pin as memory", message: "Memory name", placeholder: "e.g. deploy-checklist" });
    if (!name || !name.trim()) return;
    try {
      const path = await createMemoryEntry(name.trim(), message.content, activeProject?.root ?? undefined);
      pushToast({ title: "pinned", body: path, kind: "success", ttlMs: 4000 });
      playSound("tick");
    } catch (e) {
      pushToast({ title: "pin failed", body: humanizeError(e), kind: "error" });
    }
  };

  const isAssistant = message.role === "assistant";

  // Hide the row entirely while the message is still streaming so half-formed
  // text doesn't get copied or pinned.
  if (message.pending) return null;

  return (
    <div className="message-actions" role="toolbar" aria-label="message actions" data-session={sessionId}>
      <button
        type="button"
        className="action-btn"
        onClick={() => void handleCopy()}
        title="Copy message"
        aria-label="Copy"
      >
        Copy
      </button>
      {isAssistant && (
        <button
          type="button"
          className="action-btn"
          onClick={handleRegenerate}
          title="Regenerate from the previous user message"
          aria-label="Regenerate"
        >
          Regenerate
        </button>
      )}
      <button
        type="button"
        className="action-btn"
        onClick={() => void handleBranch()}
        title="Branch into a new session up to this message"
        aria-label="Branch"
      >
        Branch
      </button>
      <button
        type="button"
        className="action-btn"
        onClick={() => void handlePin()}
        title="Pin this message to memory"
        aria-label="Pin"
      >
        Pin
      </button>
    </div>
  );
}
