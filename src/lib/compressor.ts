import type { Message } from "@/state/store";

/**
 * Number of most-recent messages to preserve verbatim when compacting.
 * Anything older than this window gets folded into a single synthetic
 * summary system message.
 */
export const KEEP_RECENT = 8;

/**
 * Returns true when the conversation has grown past the configured
 * threshold and the user should be nudged to compact older turns.
 */
export function shouldCompact(msgCount: number, threshold: number): boolean {
  return msgCount > threshold;
}

/**
 * Build a synthetic system message that summarises the oldest portion of
 * a conversation. The caller is expected to splice this in place of all
 * messages except the last KEEP_RECENT.
 *
 * Heuristics used (kept intentionally cheap — no LLM call):
 *   - topics: first ~60 chars of each user message
 *   - decisions: assistant turns that look like resolutions (last line)
 *   - artifacts: unique file paths surfaced via `file_edit` tool events
 *   - approximate token total from preserved `totalTokens` fields
 */
export function buildSummaryMessage(messages: Message[]): Message {
  const topics: string[] = [];
  const decisions: string[] = [];
  const artifacts = new Set<string>();
  let tokenTotal = 0;

  for (const m of messages) {
    if (m.totalTokens != null) tokenTotal += m.totalTokens;

    if (m.role === "user") {
      const t = m.content.trim().replace(/\s+/g, " ").slice(0, 60);
      if (t.length > 0) topics.push(t);
    } else if (m.role === "assistant") {
      const lines = m.content.trim().split("\n").filter((l) => l.trim().length > 0);
      const tail = lines[lines.length - 1];
      if (tail && /\b(done|fixed|added|implemented|created|updated|wrote|removed|refactored)\b/i.test(tail)) {
        decisions.push(tail.slice(0, 80));
      }
    }

    for (const t of m.tools) {
      if (t.name === "file_edit" && t.preview) {
        const path = t.preview.split("\n")[0]?.trim();
        if (path) artifacts.add(path);
      }
    }
  }

  const bulletize = (items: string[]): string => {
    if (items.length === 0) return "  (none)";
    return items.map((s) => `  - ${s}`).join("\n");
  };

  const body =
    `📚 Summary of ${messages.length} earlier turns:\n` +
    `- topics:\n${bulletize(topics)}\n` +
    `- decisions:\n${bulletize(decisions)}\n` +
    `- artifacts:\n${bulletize(Array.from(artifacts))}\n` +
    `- approx tokens folded: ${tokenTotal.toLocaleString()}`;

  return {
    id: `compact-${crypto.randomUUID()}`,
    role: "system",
    content: body,
    tools: [],
  };
}
