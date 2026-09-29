// Static composer vocabulary mirroring the desktop (src/lib/at-vocab.ts and
// src/lib/slash-commands.ts). @-tokens are expanded server-side by the chat
// path (`expand_at_tokens` in commands/chat.rs), so they work from the phone.
// Slash commands are client-side on the desktop; the phone runs the handful
// that make sense locally and tells you about the rest.

export interface AtToken {
  /** What gets inserted (a trailing ":" means "type the argument"). */
  insert: string;
  label: string;
  hint: string;
}

export const AT_TOKENS: AtToken[] = [
  { insert: "@diff", label: "@diff", hint: "Working-tree diff" },
  { insert: "@status", label: "@status", hint: "git status" },
  { insert: "@log", label: "@log", hint: "Recent commits" },
  { insert: "@recent", label: "@recent", hint: "Recently edited files" },
  { insert: "@codebase", label: "@codebase", hint: "Semantic code search" },
  { insert: "@repomap", label: "@repomap", hint: "Repository map" },
  { insert: "@tree", label: "@tree", hint: "Directory tree" },
  { insert: "@cwd", label: "@cwd", hint: "Project root" },
  { insert: "@env", label: "@env", hint: "Environment summary" },
  { insert: "@problems", label: "@problems", hint: "Diagnostics" },
  { insert: "@terminal", label: "@terminal", hint: "Last terminal output" },
  { insert: "@brain", label: "@brain", hint: "Project memory" },
  { insert: "@docs", label: "@docs", hint: "Project docs retrieval" },
  { insert: "@folder:", label: "@folder:<path>", hint: "Folder contents" },
  { insert: "@outline:", label: "@outline:<file>", hint: "File outline" },
  { insert: "@def:", label: "@def:<symbol>", hint: "Definition" },
  { insert: "@refs:", label: "@refs:<symbol>", hint: "References" },
  { insert: "@grep:", label: "@grep:<pattern>", hint: "Search the repo" },
  { insert: "@blame:", label: "@blame:<file>", hint: "git blame" },
  { insert: "@memory:", label: "@memory:<path>", hint: "One memory note" },
  { insert: "@frag:", label: "@frag:<name>", hint: "Prompt fragment" },
  { insert: "@web:", label: "@web:<url>", hint: "Fetch a page" },
  { insert: "@websearch:", label: "@websearch:<query>", hint: "Web search" },
  { insert: "@", label: "@<path>", hint: "Attach a file by path" },
];

export interface SlashCommand {
  name: string;
  description: string;
  /** Runs on the phone. Others are desktop-only and not sent. */
  local: boolean;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "clear", description: "Start a new chat", local: true },
  { name: "stop", description: "Stop the running reply", local: true },
  {
    name: "help",
    description: "Show what the composer understands",
    local: true,
  },
  { name: "compact", description: "Condense the conversation", local: false },
  { name: "resume", description: "Resume the previous chat", local: false },
  { name: "tokens", description: "Context breakdown", local: false },
  { name: "note", description: "Save a note to project memory", local: false },
  { name: "export", description: "Export the conversation", local: false },
  { name: "snapshot", description: "Create a checkpoint", local: false },
  { name: "undo", description: "Revert the last edit", local: false },
  { name: "web", description: "Fetch a URL into context", local: false },
  { name: "research", description: "Deep research on a topic", local: false },
  { name: "architect", description: "Plan before editing", local: false },
  { name: "apply", description: "Apply the architect plan", local: false },
  { name: "run", description: "Run a shell command", local: false },
  { name: "lint", description: "Run the linter", local: false },
  { name: "add", description: "Pin a file to context", local: false },
  { name: "drop", description: "Unpin a file", local: false },
  { name: "ls", description: "List pinned files", local: false },
  { name: "focus", description: "Focus a sub-folder", local: false },
];

/** The "/xyz" or "@xyz" token the caret is currently inside, if any. */
export function activeToken(
  text: string,
  caret: number,
): { kind: "slash" | "at"; start: number; query: string } | null {
  const upTo = text.slice(0, caret);
  if (/^\/[a-z-]*$/i.test(upTo)) {
    return { kind: "slash", start: 0, query: upTo.slice(1).toLowerCase() };
  }
  const m = /(?:^|\s)(@[^\s]*)$/.exec(upTo);
  if (m) {
    const start = caret - m[1].length;
    return { kind: "at", start, query: m[1].slice(1).toLowerCase() };
  }
  return null;
}
