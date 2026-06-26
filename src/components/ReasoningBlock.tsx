import { useCortexStore } from "@/state/store";
import { Chevron } from "@/lib/chevron";

interface ReasoningBlockProps {
  reasoning: string;
  messageId: string;
}

/**
 * Collapsible "thinking" section shown beneath an assistant message.
 *
 * Collapsed by default; toggles expansion via a per-message slice in the
 * Zustand store (`expandedReasonings`). The expanded state is session-only —
 * intentionally not persisted to localStorage.
 */
export function ReasoningBlock({ reasoning, messageId }: ReasoningBlockProps) {
  const expanded = useCortexStore((s) => s.expandedReasonings.has(messageId));
  const toggle = useCortexStore((s) => s.toggleReasoning);

  // Count non-empty lines so the user has a sense of length before expanding.
  const lineCount = reasoning.split("\n").filter((l) => l.length > 0).length;

  return (
    <div className="reasoning-block">
      <button
        type="button"
        className="reasoning-toggle"
        onClick={() => toggle(messageId)}
        aria-expanded={expanded}
      >
        <Chevron open={expanded} size={13} />
        <span>
          {expanded
            ? `Hide thinking`
            : `Show thinking (${lineCount} line${lineCount === 1 ? "" : "s"})`}
        </span>
      </button>
      {expanded && <pre className="reasoning-body">{reasoning}</pre>}
    </div>
  );
}
