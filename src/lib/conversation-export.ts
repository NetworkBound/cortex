/**
 * Export the current chat conversation (live store `Message[]`) to disk as
 * Markdown or JSON.
 *
 * Pure formatters (`conversationToMarkdown` / `conversationToJson`) plus a
 * thin `exportConversation` orchestrator that computes a filename, joins it
 * to the active project root, and forwards to `saveFileText`.
 *
 * Additive only — no existing behaviour is touched. The renderer can't easily
 * resolve $HOME, so exports are written under the active project root; if no
 * project is active `exportConversation` throws a clear Error for the caller
 * to toast.
 */
import type { Message } from "@/state/store";
import { saveFileText } from "@/lib/editor-save";

export interface ConversationExportMeta {
  sessionId?: string;
  project?: string;
}

/** Roles we render in an export. `error` rows and pending rows are skipped. */
function isExportable(m: Message): boolean {
  if (m.pending) return false;
  return m.role === "user" || m.role === "assistant" || m.role === "system";
}

/** Human-facing header label for a message role. */
function roleLabel(m: Message): string {
  if (m.role === "user") return "You";
  if (m.role === "assistant") {
    return m.agent ? `Assistant (${m.agent})` : "Assistant";
  }
  // system
  return "System";
}

/**
 * Render the conversation as a clean Markdown transcript: a title header
 * followed by one `### <Role>` section per (non-error, non-pending) message.
 * Fenced code in `content` is preserved verbatim.
 */
export function conversationToMarkdown(
  messages: Message[],
  meta: ConversationExportMeta,
): string {
  const header: string[] = ["# Cortex Chat Transcript", ""];
  if (meta.project) header.push(`- project: \`${meta.project}\``);
  if (meta.sessionId) header.push(`- session: \`${meta.sessionId}\``);
  header.push(`- exported: ${new Date().toISOString()}`, "", "---", "");

  const body = messages
    .filter(isExportable)
    .map((m) => `### ${roleLabel(m)}\n\n${m.content}\n`)
    .join("\n");

  return header.join("\n") + body;
}

/** Stable serialisable shape for the JSON export. */
interface ExportJsonMessage {
  role: Message["role"];
  agent: string | null;
  content: string;
  runId: string | null;
}

/**
 * Render the conversation as pretty-printed JSON with a stable shape:
 * `{ exportedAt, sessionId, project, messages: [{role, agent, content, runId}] }`.
 * Skips error + pending rows for parity with the Markdown export.
 */
export function conversationToJson(
  messages: Message[],
  meta: ConversationExportMeta,
): string {
  const payload = {
    exportedAt: new Date().toISOString(),
    sessionId: meta.sessionId ?? null,
    project: meta.project ?? null,
    messages: messages.filter(isExportable).map<ExportJsonMessage>((m) => ({
      role: m.role,
      agent: m.agent ?? null,
      content: m.content,
      runId: m.runId ?? null,
    })),
  };
  return JSON.stringify(payload, null, 2);
}

/** Strip characters that are awkward in filenames; keep it conservative. */
function sanitizeSlug(raw: string): string {
  return raw.replace(/[^A-Za-z0-9._-]/g, "-").replace(/-+/g, "-");
}

/** Join path segments with forward slashes (backend accepts them). */
function joinPath(...parts: string[]): string {
  return parts
    .map((p) => p.replace(/[\\/]+$/g, ""))
    .filter((p) => p.length > 0)
    .join("/");
}

/**
 * Build the transcript body and write it to disk.
 *
 * Filename is `cortex-chat-<sessionId-or-timestamp>.<ext>`, written directly
 * under the active project root. If there is genuinely no project, throws —
 * the caller surfaces the message via a toast.
 *
 * @returns the absolute path written.
 */
export async function exportConversation(
  format: "md" | "json",
  messages: Message[],
  meta: ConversationExportMeta,
  defaultDir?: string,
): Promise<string> {
  const baseDir = defaultDir ?? meta.project;
  if (!baseDir) {
    throw new Error(
      "No active project — open a project before exporting the conversation.",
    );
  }

  const body =
    format === "md"
      ? conversationToMarkdown(messages, meta)
      : conversationToJson(messages, meta);

  // Always include a timestamp so re-exporting the same session does not
  // silently overwrite a prior export. The sessionId (when present) keeps the
  // filename recognisable; the timestamp guarantees uniqueness per export.
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const stamp = meta.sessionId
    ? `${meta.sessionId.slice(-12)}-${timestamp}`
    : timestamp;
  const fileName = `cortex-chat-${sanitizeSlug(stamp)}.${format}`;

  // Written directly under the project root: the backend `save_file_text`
  // does not create parent directories, so a subfolder would fail on first use.
  const path = joinPath(baseDir, fileName);

  return await saveFileText(path, body);
}
