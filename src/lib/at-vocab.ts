/**
 * @-vocabulary backend for the Cursor-style multi-typed picker.
 *
 * WIRING NOTES (for ChatPane.tsx — DO NOT TOUCH HERE, this file only):
 *   - The picker now emits values in one of two shapes:
 *       1. A plain filename / relative path (existing `files` behavior).
 *       2. A vocab envelope of the form `<kind>:<value>` where <kind> is one of
 *          files | folders | git | recent | docs | memory.
 *     For now ChatPane.tsx's `insertFile(name)` should keep treating the picked
 *     value as a plain string; once you're ready, parse with:
 *         const m = name.match(/^(files|folders|git|recent|docs|memory):(.+)$/);
 *     and route the kind into your envelope/context-builder.
 *   - The picker also auto-switches active kind when the user types
 *     `Files/`, `Folders/`, `Git`, `Recent`, `Docs`, or `Memory` after the `@`.
 *     ChatPane should NOT special-case these prefixes; just pass the raw query
 *     after `@` straight into FilePicker as today.
 */

import { invoke } from "@tauri-apps/api/core";
import { projectFiles, type FileTreeEntry } from "@/lib/projects";
import { listMemoryFiles, type MemoryFile } from "@/lib/memory";
import {
  recentTraces,
  recentIssues,
  recentCrashes,
  type Trace,
  type IssueRow,
  type CrashRow,
} from "@/lib/observability";
import { brainSnapshot } from "@/lib/brain";
import { timeAgo } from "@/lib/checkpoints";
import { listSnippets } from "@/lib/snippets";
import {
  gitWorkingDiff,
  projectDiagnostics,
  recentTerminalOutput,
} from "@/lib/context";

export type VocabKind =
  | "files"
  | "folders"
  | "git"
  | "recent"
  | "docs"
  | "memory"
  | "symbols"
  | "threads"
  | "diagnostics"
  | "snippets"
  | "diff"
  | "problems"
  | "terminal"
  | "brain"
  | "status"
  | "recent-edits"
  | "frag"
  | "web"
  | "websearch"
  | "grep"
  | "codebase"
  | "cwd"
  | "tree"
  | "outline"
  | "def"
  | "refs"
  | "env";

interface SymbolHit {
  path: string;
  name: string;
  kind: string;
  line: number;
}

export interface VocabEntry {
  kind: VocabKind;
  /** Display label (short, fits in the picker row). */
  label: string;
  /** The actual value to insert into the chat input (relative path or envelope). */
  value: string;
  /** Optional secondary line (e.g. dir path, source, snippet). */
  preview?: string;
}

const MAX_PER_KIND = 20;

export const VOCAB_KINDS: { kind: VocabKind; label: string; hint: string }[] = [
  { kind: "files", label: "Files", hint: "project files" },
  { kind: "folders", label: "Folders", hint: "inline a folder's files (@folder:path)" },
  { kind: "symbols", label: "Symbols", hint: "functions, classes, structs" },
  { kind: "git", label: "Git", hint: "git context — diff, status, log, blame, env" },
  { kind: "recent", label: "Recent", hint: "recent traces" },
  { kind: "docs", label: "Docs", hint: "@docs retrieves project docs · notes below" },
  { kind: "memory", label: "Memory", hint: "memory entries" },
  { kind: "threads", label: "Threads", hint: "recent chat sessions" },
  { kind: "diagnostics", label: "Diagnostics", hint: "recent issues & crashes" },
  { kind: "snippets", label: "Snippets", hint: "saved prompt snippets" },
  { kind: "diff", label: "Diff", hint: "git working diff" },
  { kind: "problems", label: "Problems", hint: "compile errors & warnings" },
  { kind: "terminal", label: "Terminal", hint: "recent terminal output" },
  { kind: "brain", label: "Brain", hint: "auto-attach top brain hits for the draft" },
  { kind: "status", label: "Git status", hint: "git status --short of active project" },
  { kind: "recent-edits", label: "Recent edits", hint: "last 8 modified files in active project" },
  { kind: "frag", label: "Fragment", hint: "reusable prompt snippet from ~/.cortex/fragments/" },
  { kind: "web", label: "Web", hint: "fetch URL and inline as text" },
  { kind: "websearch", label: "Web search", hint: "live web search results (@websearch:query)" },
  { kind: "grep", label: "Grep", hint: "recursive case-insensitive search of project" },
  { kind: "codebase", label: "Codebase", hint: "semantic retrieval ranked by your message" },
  { kind: "cwd", label: "cwd", hint: "top-level file tree of active project" },
  { kind: "tree", label: "Tree", hint: "directory tree of active project (depth 2; @tree:N to tune)" },
  { kind: "outline", label: "Outline", hint: "symbol outline of a file (@outline:path/to/file)" },
  { kind: "def", label: "Definition", hint: "jump to a symbol's definition + body (@def:name)" },
  { kind: "refs", label: "References", hint: "find all uses of a symbol across the project (@refs:name)" },
  { kind: "env", label: "env", hint: "project root + git HEAD + branch" },
];

/**
 * One `@`-mention provider, flattened for documentation. The Help panel renders
 * its reference *live* from this registry (the same way the Slash-commands
 * section renders from `COMMANDS`), so the documented vocabulary can never drift
 * from what the composer actually resolves.
 *
 * Add a new `@`-provider? Add it here too and it shows up in Help automatically.
 * The e2e probe (`exerciseAtVocabReferenceFlow`) asserts every flagship provider
 * paints a row, so a built-but-undocumented provider fails the render test — the
 * exact regression this registry exists to prevent (`@codebase` had shipped
 * fully wired yet absent from Help).
 */
export interface AtProvider {
  /** The token shape a user types, e.g. `@def:<symbol>`. */
  syntax: string;
  /** One-line description of what it injects. */
  summary: string;
  /** Equivalent token spellings, if any. */
  aliases?: string[];
  /** Bucket the Help panel groups under; one of `AT_PROVIDER_CATEGORIES`. */
  category: (typeof AT_PROVIDER_CATEGORIES)[number];
}

/** Display order of the `@`-provider category buckets in the Help panel. */
export const AT_PROVIDER_CATEGORIES = [
  "Files & layout",
  "Code & symbols",
  "Search & web",
  "Git",
  "Diagnostics",
  "Memory & docs",
] as const;

/**
 * The full `@`-mention vocabulary, each entry verified against a live backend
 * handler in `expand_at_tokens` (Rust). Keep this in sync with the resolver:
 * documenting a token with no handler would manufacture the very dead-end this
 * file guards against.
 */
export const AT_PROVIDERS: AtProvider[] = [
  // ── Files & layout ────────────────────────────────────────────────────────
  {
    syntax: "@<path>",
    summary:
      "Inline any file as context (≤200KB). Dropping a bare path like src/auth.rs into the draft also auto-attaches it (up to 3, Aider-style).",
    category: "Files & layout",
  },
  {
    syntax: "@folder:<path>",
    aliases: ["@dir:<path>"],
    summary:
      "Inline the text files directly inside one folder (one level, not recursive) so the model reads a whole module at once — vs @<path> (one file) / @tree (layout only). Binaries are listed but not inlined.",
    category: "Files & layout",
  },
  {
    syntax: "@tree",
    summary:
      "Ignore-aware directory tree of the active project (depth 2; @tree:N tunes 1–6) — shows layout, vs @cwd's single level.",
    category: "Files & layout",
  },
  {
    syntax: "@cwd",
    aliases: ["@ls"],
    summary: "Top-level file tree of the active project (one level deep).",
    category: "Files & layout",
  },
  {
    syntax: "@env",
    summary: "Project root + git HEAD + branch — a one-line orientation block.",
    category: "Files & layout",
  },

  // ── Code & symbols ────────────────────────────────────────────────────────
  {
    syntax: "@codebase",
    summary:
      "Continue-style semantic retrieval over the whole project, ranked by your message — surfaces the most relevant code without manual grepping (@codebase:N tunes the hit count 1–50).",
    category: "Code & symbols",
  },
  {
    syntax: "@repomap",
    summary:
      "Compressed symbol map of the project (fn/struct/class with line numbers); files annotated with ★N.NN PageRank scores and personalized by identifiers in your message so task-relevant files float to the top.",
    category: "Code & symbols",
  },
  {
    syntax: "@outline:<file>",
    summary:
      "Zed-style symbol outline of one file (functions/classes/headings + line numbers + signatures; markdown headings nested as a ToC), vs @repomap's cross-file ranking.",
    category: "Code & symbols",
  },
  {
    syntax: "@def:<symbol>",
    aliases: ["@symbol:<symbol>"],
    summary:
      "Go to definition: the declaration site(s) of a named symbol across the project + the body (path:line + line-numbered code), vs @outline (one file) / @grep (literal text).",
    category: "Code & symbols",
  },
  {
    syntax: "@refs:<symbol>",
    aliases: ["@callers:<symbol>", "@uses:<symbol>"],
    summary:
      "Zed-style find all references: every use of a symbol across the project, grouped by file with the declaration marked. Whole-word + case-sensitive, unlike @grep's substring search; the companion to @def.",
    category: "Code & symbols",
  },

  // ── Search & web ──────────────────────────────────────────────────────────
  {
    syntax: "@grep:<pattern>",
    summary: "Recursive case-insensitive search across source files (50 hits max).",
    category: "Search & web",
  },
  {
    syntax: "@web:<url>",
    summary: "Fetch a URL and inline it as text (8s timeout, 60KB cap).",
    category: "Search & web",
  },
  {
    syntax: "@websearch:<query>",
    aliases: ["@search:<query>", "@google:<query>"],
    summary:
      "Live keyless web search: inline the top 6 ranked results (title · url · snippet) so the model can cite them, vs @web: (one known URL). Works with any model — injected as context, no tool channel needed.",
    category: "Search & web",
  },

  // ── Git ───────────────────────────────────────────────────────────────────
  {
    syntax: "@diff",
    summary: "git diff vs HEAD of the active project.",
    category: "Git",
  },
  {
    syntax: "@status",
    summary: "git status --short of the active project.",
    category: "Git",
  },
  {
    syntax: "@log",
    summary: "Last N git commits, oneline (@log:N; defaults to 20, max 200).",
    category: "Git",
  },
  {
    syntax: "@blame:<file>",
    summary:
      "git blame for one file: per-line authorship (sha · author · line), capped at 400 lines, vs @log (whole-repo commit list).",
    category: "Git",
  },
  {
    syntax: "@recent",
    summary: "Last 8 modified files in the active project (@recent:N for 1–50).",
    category: "Git",
  },

  // ── Diagnostics ───────────────────────────────────────────────────────────
  {
    syntax: "@problems",
    aliases: ["@diagnostics", "@lint"],
    summary:
      "Continue-style compiler diagnostics: runs the project's check-only compilers (cargo check / tsc --noEmit, cached 30s) and inlines current errors & warnings so the model can fix them. Reports 'No problems' when clean.",
    category: "Diagnostics",
  },
  {
    syntax: "@terminal",
    summary:
      "Recent terminal output from the active session (@terminal:N tunes the line count).",
    category: "Diagnostics",
  },

  // ── Memory & docs ─────────────────────────────────────────────────────────
  {
    syntax: "@brain",
    summary:
      "Auto-attach the top brain hits for your draft (@brain:N for 1–10). The local brain also auto-fires 800ms after a typing pause once the draft is ≥25 chars.",
    category: "Memory & docs",
  },
  {
    syntax: "@memory:<path>",
    summary:
      'Inline a file, signalling "this came from memory" — the same payload as @<path> with a memory-provenance label.',
    category: "Memory & docs",
  },
  {
    syntax: "@docs",
    summary:
      "Continue-style documentation retrieval: ranks the project's docs by your message and inlines the most relevant sections (@docs:N tunes the count), vs @codebase (code) / @memory (one note).",
    category: "Memory & docs",
  },
  {
    syntax: "@frag:<name>",
    summary: "Inline a reusable prompt fragment from ~/.cortex/fragments/<name>.md.",
    category: "Memory & docs",
  },
];

const KEYWORD_TO_KIND: Record<string, VocabKind> = {
  files: "files",
  file: "files",
  folders: "folders",
  folder: "folders",
  dir: "folders",
  dirs: "folders",
  git: "git",
  recent: "recent",
  recents: "recent",
  recentchanges: "recent",
  trace: "recent",
  traces: "recent",
  docs: "docs",
  doc: "docs",
  memory: "memory",
  mem: "memory",
  symbols: "symbols",
  symbol: "symbols",
  sym: "symbols",
  syms: "symbols",
  fn: "symbols",
  func: "symbols",
  class: "symbols",
  thread: "threads",
  threads: "threads",
  diagnostic: "diagnostics",
  diag: "diagnostics",
  error: "diagnostics",
  issue: "diagnostics",
  crash: "diagnostics",
  snippet: "snippets",
  snippets: "snippets",
  s: "snippets",
  sn: "snippets",
  diff: "diff",
  diffs: "diff",
  changes: "diff",
  problems: "problems",
  problem: "problems",
  errors: "problems",
  warnings: "problems",
  compile: "problems",
  terminal: "terminal",
  term: "terminal",
  shell: "terminal",
  stdout: "terminal",
  codebase: "codebase",
  code: "codebase",
  cb: "codebase",
  retrieve: "codebase",
  tree: "tree",
  dirtree: "tree",
  layout: "tree",
  outline: "outline",
  toc: "outline",
  def: "def",
  definition: "def",
  goto: "def",
  gotodef: "def",
  refs: "refs",
  references: "refs",
  callers: "refs",
  usages: "refs",
  uses: "refs",
  findrefs: "refs",
  websearch: "websearch",
  search: "websearch",
  google: "websearch",
  ddg: "websearch",
};

/**
 * Given the @-token text (everything after the `@`, e.g. `Files/`, `Folders/run`,
 * `Git`, `Recent`, `Docs/note`, `Memory`, `:func`), return the implied VocabKind.
 * A leading `:` is the shorthand trigger for `symbols` (so `@:render` searches
 * functions/classes named "render"). Default is `files`.
 */
export function detectVocab(s: string): VocabKind {
  if (!s) return "files";
  if (s.startsWith(":")) return "symbols";
  // A vocab keyword must either be the entire token or be followed by a `/`
  // or `:` delimiter (matching `stripVocabPrefix`). Whitespace does NOT
  // delimit a keyword, so plain searches like `error handler` stay `files`.
  const m = s.match(/^([A-Za-z]+)(?:[/:]|$)/);
  const head = m?.[1].toLowerCase() ?? "";
  if (!head) return "files";
  return KEYWORD_TO_KIND[head] ?? "files";
}

/**
 * Strip a leading vocab keyword + optional `/`, `:` or whitespace from `s`,
 * returning the remaining query. e.g. `Folders/run` → `run`, `Files` → ``,
 * `:render` → `render`, `Symbols:render` → `render`.
 */
export function stripVocabPrefix(s: string): string {
  if (!s) return "";
  // `:foo` shorthand for symbols.
  if (s.startsWith(":")) return s.slice(1);
  // Mirror `detectVocab`: only strip a keyword that is the whole token or is
  // followed by a `/` or `:` delimiter (whitespace does not delimit).
  const m = s.match(/^([A-Za-z]+)(?:[/:]+(.*))?$/);
  if (!m) return s;
  const head = m[1].toLowerCase();
  if (KEYWORD_TO_KIND[head]) return m[2] ?? "";
  return s;
}

function fuzzyMatch(name: string, q: string): boolean {
  if (!q) return true;
  return name.toLowerCase().includes(q.toLowerCase());
}

function envelope(
  kind:
    | VocabKind
    | "thread"
    | "diagnostic"
    | "snippet"
    | "diff"
    | "problem"
    | "terminal"
    | "skill",
  value: string,
): string {
  return `${kind}:${value}`;
}

async function fetchFiles(
  query: string,
  root: string | null,
  isDir: boolean,
): Promise<VocabEntry[]> {
  if (!root) return [];
  let entries: FileTreeEntry[] = [];
  try {
    entries = await projectFiles(root, 500);
  } catch {
    return [];
  }
  const filtered = entries
    .filter((f) => f.is_dir === isDir && fuzzyMatch(f.name, query))
    .slice(0, MAX_PER_KIND);
  const kind: VocabKind = isDir ? "folders" : "files";
  // Folders insert a real `@folder:<relpath>` provider token (the backend
  // inlines the folder's text files); files keep the legacy bare-name insert.
  const relOf = (full: string): string =>
    full.startsWith(root)
      ? full.slice(root.length).replace(/^[/\\]+/, "")
      : full.split(/[/\\]/).pop() || full;
  return filtered.map((f) => ({
    kind,
    label: f.name,
    value: isDir ? `folder:${relOf(f.path)}` : f.name,
    preview: f.path,
  }));
}

async function fetchRecent(query: string): Promise<VocabEntry[]> {
  let traces: Trace[] = [];
  try {
    traces = await recentTraces(20);
  } catch {
    return [];
  }
  const labelled = traces.map((t) => {
    const attr = t.spans.find((s) => s.name === "chat.turn")?.attributes as
      | { first_message_preview?: string }
      | undefined;
    const title =
      attr?.first_message_preview?.slice(0, 80) ||
      `trace ${t.trace_id.slice(0, 10)}`;
    return { trace_id: t.trace_id, title, session: t.session_id };
  });
  return labelled
    .filter((t) => fuzzyMatch(t.title, query))
    .slice(0, MAX_PER_KIND)
    .map((t) => ({
      kind: "recent" as const,
      label: t.title,
      value: envelope("recent", `trace:${t.trace_id}`),
      preview: `session ${t.session.slice(0, 10)}`,
    }));
}

async function fetchMemory(
  query: string,
  root: string | null,
): Promise<VocabEntry[]> {
  let mem: MemoryFile[] = [];
  try {
    mem = await listMemoryFiles(root ?? undefined);
  } catch {
    return [];
  }
  return mem
    .filter((m) => fuzzyMatch(m.name, query))
    .slice(0, MAX_PER_KIND)
    .map((m) => ({
      kind: "memory" as const,
      label: m.name,
      value: envelope("memory", m.path),
      preview: m.source,
    }));
}

async function fetchDocs(query: string): Promise<VocabEntry[]> {
  let mem: MemoryFile[] = [];
  try {
    mem = await listMemoryFiles();
  } catch {
    return [];
  }
  const notes = mem
    .filter((m) => {
      const src = (m.source ?? "").toLowerCase();
      return src.includes("cortex brain") || src.includes("obsidian");
    })
    .filter((m) => fuzzyMatch(m.name, query))
    .slice(0, MAX_PER_KIND)
    .map((m) => ({
      kind: "docs" as const,
      label: m.name,
      value: envelope("docs", m.path),
      preview: m.source,
    }));
  // Continue-style documentation-retrieval provider: a bare `@docs` token the
  // backend resolves into the project's most relevant doc *sections*, ranked by
  // the rest of the message. Distinct from the brain/obsidian *note* references
  // below (those insert a file path). Offered first when the query is empty.
  if (!query.trim()) {
    return [
      {
        kind: "docs" as const,
        label: "@docs",
        value: "docs",
        preview: "Retrieve relevant project documentation, ranked by your message",
      },
      ...notes,
    ];
  }
  return notes;
}

const SYMBOL_KIND_BADGES: Record<string, string> = {
  fn: "fn",
  class: "class",
  struct: "struct",
  interface: "iface",
  enum: "enum",
  trait: "trait",
  type: "type",
  const: "const",
  heading: "h",
};

function symbolBadge(kind: string): string {
  return SYMBOL_KIND_BADGES[kind] ?? kind;
}

async function fetchSymbols(
  query: string,
  root: string | null,
): Promise<VocabEntry[]> {
  if (!root) return [];
  let hits: SymbolHit[] = [];
  try {
    hits = await invoke<SymbolHit[]>("repo_symbols", {
      root,
      query,
      limit: MAX_PER_KIND,
    });
  } catch {
    return [];
  }
  return hits.slice(0, MAX_PER_KIND).map((h) => ({
    kind: "symbols" as const,
    label: `${symbolBadge(h.kind)} ${h.name}`,
    value: envelope("symbols", `${h.path}:${h.line}`),
    preview: `${h.path}:${h.line}`,
  }));
}

/**
 * The Git namespace is a curated menu of the real, backend-resolved git
 * context providers (see `resolve_special_token` in commands/chat.rs). Each
 * `value` inserts a working `@…` token via ChatPane's default insert path —
 * NOT the old `@git:<.git/logs path>` envelope, which the backend never
 * resolved (a dead token + a "[git context not yet wired]" placeholder, the
 * exact dead-end this replaces). `keys` widens the fuzzy match so e.g.
 * "history" finds @log and "author" finds @blame.
 */
const GIT_PROVIDERS: { label: string; value: string; preview: string; keys: string }[] = [
  {
    label: "@status",
    value: "status",
    preview: "git status --short — staged, unstaged & untracked changes",
    keys: "status changes state",
  },
  {
    label: "@diff",
    value: "diff",
    preview: "git working diff — every uncommitted change in the tree",
    keys: "diff changes working uncommitted",
  },
  {
    label: "@log",
    value: "log",
    preview: "Recent commits, oneline (last 20; @log:N for 1–200)",
    keys: "log commits history recent",
  },
  {
    label: "@blame:<file>",
    value: "blame:",
    preview: "Per-line authorship for a file — git blame (sha · author · line)",
    keys: "blame author authorship who",
  },
  {
    label: "@env",
    value: "env",
    preview: "Project root + git HEAD + branch (one-line orientation)",
    keys: "env head branch root",
  },
];

async function fetchGit(
  query: string,
  root: string | null,
): Promise<VocabEntry[]> {
  // Every git provider needs a project root to resolve server-side; with none
  // configured, offering them would insert tokens that silently drop. Mirror
  // fetchFiles/fetchFolders and return nothing so the menu shows its empty
  // state instead of a dead-end placeholder.
  if (!root) return [];
  return GIT_PROVIDERS.filter((p) => fuzzyMatch(`${p.label} ${p.keys}`, query)).map(
    (p) => ({
      kind: "git" as const,
      label: p.label,
      value: p.value,
      preview: p.preview,
    }),
  );
}

async function fetchThreads(query: string): Promise<VocabEntry[]> {
  let snap;
  try {
    snap = await brainSnapshot();
  } catch {
    return [];
  }
  const sessions = snap.recent_sessions ?? [];
  return sessions
    .filter((s) => {
      const hay = `${s.first_message ?? ""} ${s.session_id}`;
      return fuzzyMatch(hay, query);
    })
    .slice(0, MAX_PER_KIND)
    .map((s) => {
      const label =
        (s.first_message && s.first_message.trim()) ||
        s.session_id.slice(-12);
      const preview = `${timeAgo(s.last_active_ms)} · ${s.message_count} msgs`;
      return {
        kind: "threads" as const,
        label: label.slice(0, 80),
        value: envelope("thread", s.session_id),
        preview,
      };
    });
}

interface DiagHit {
  ts: number;
  kind: "issue" | "crash";
  badge: string;
  message: string;
  id: string;
}

async function fetchDiagnostics(query: string): Promise<VocabEntry[]> {
  let issues: IssueRow[] = [];
  let crashes: CrashRow[] = [];
  try {
    [issues, crashes] = await Promise.all([
      recentIssues(20),
      recentCrashes(20),
    ]);
  } catch {
    return [];
  }
  const hits: DiagHit[] = [
    ...issues.map((i) => ({
      ts: i.last_seen,
      kind: "issue" as const,
      badge: "issue",
      message: i.message,
      id: i.fingerprint,
    })),
    ...crashes.map((c) => ({
      ts: c.ts,
      kind: "crash" as const,
      badge: c.kind || "crash",
      message: c.message,
      id: String(c.id),
    })),
  ];
  hits.sort((a, b) => b.ts - a.ts);
  return hits
    .filter((h) => fuzzyMatch(h.message, query))
    .slice(0, MAX_PER_KIND)
    .map((h) => ({
      kind: "diagnostics" as const,
      label: `[${h.badge}] ${h.message.slice(0, 80)}`,
      value: envelope("diagnostic", h.id),
      preview: timeAgo(h.ts),
    }));
}

async function fetchSnippetsVocab(query: string): Promise<VocabEntry[]> {
  let snippets;
  try {
    snippets = await listSnippets();
  } catch {
    return [];
  }
  return snippets
    .filter((s) => fuzzyMatch(s.name, query))
    .slice(0, MAX_PER_KIND)
    .map((s) => {
      const preview = s.body.replace(/\s+/g, " ").trim().slice(0, 80);
      return {
        kind: "snippets" as const,
        label: s.name,
        value: envelope("snippet", s.name),
        preview: preview || "(empty snippet)",
      };
    });
}

/**
 * Unquote a git-style path. When a path contains special characters git wraps
 * it in double quotes and C-escapes it (e.g. `"a/foo\tbar"`). We decode the
 * common escapes; non-quoted paths are returned as-is.
 */
function unquoteGitPath(p: string): string {
  if (p.length < 2 || p[0] !== '"' || p[p.length - 1] !== '"') return p;
  const inner = p.slice(1, -1);
  return inner.replace(/\\(["\\nt]|[0-7]{1,3})/g, (_, esc: string) => {
    switch (esc) {
      case '"':
        return '"';
      case "\\":
        return "\\";
      case "n":
        return "\n";
      case "t":
        return "\t";
      default:
        return String.fromCharCode(parseInt(esc, 8));
    }
  });
}

/** Strip a leading `a/` or `b/` prefix from a (already unquoted) diff path. */
function stripDiffPrefix(p: string): string {
  return p.replace(/^[ab]\//, "");
}

/**
 * Parse a `git diff` text blob into one entry per changed file. We look for
 * `diff --git` markers, then scan ahead for the first added (`+`) or removed
 * (`-`) line so the picker can show a meaningful preview without dumping the
 * entire hunk.
 *
 * The new path is resolved from the authoritative `+++ b/<new>` and
 * `rename to <new>` lines when present, falling back to parsing the
 * `diff --git` header itself. This handles renamed, quoted, and
 * space-containing paths that the simple `a/<old> b/<new>` shape misses.
 */
function parseDiffPerFile(
  diff: string,
): { path: string; preview: string }[] {
  if (!diff) return [];
  const lines = diff.split("\n");
  const out: { path: string; preview: string }[] = [];
  let currentPath: string | null = null;
  let currentPreview: string | null = null;
  const flush = () => {
    if (currentPath) {
      out.push({ path: currentPath, preview: currentPreview ?? "(no preview)" });
    }
  };
  // Best-effort path extraction from a `diff --git` header. Tries the quoted
  // form first, then an unquoted `a/<old> b/<new>` shape; if the path contains
  // spaces this may be ambiguous, but a later `+++`/`rename to` line corrects it.
  const pathFromHeader = (header: string): string | null => {
    const rest = header.slice("diff --git ".length);
    const quoted = rest.match(/^("(?:[^"\\]|\\.)*") ("(?:[^"\\]|\\.)*")$/);
    if (quoted) return stripDiffPrefix(unquoteGitPath(quoted[2]));
    const m = rest.match(/^a\/(.+) b\/(.+)$/);
    if (m) return m[2];
    return null;
  };
  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      flush();
      currentPath = pathFromHeader(line);
      currentPreview = null;
      continue;
    }
    // Authoritative new-path markers override the (possibly ambiguous) header.
    if (line.startsWith("rename to ")) {
      currentPath = unquoteGitPath(line.slice("rename to ".length));
      continue;
    }
    if (line.startsWith("+++ ")) {
      const target = line.slice("+++ ".length);
      if (target !== "/dev/null") {
        currentPath = stripDiffPrefix(unquoteGitPath(target));
      }
      continue;
    }
    if (currentPath && currentPreview === null) {
      // Skip metadata (---/@@). First real +/- line wins.
      if (line.startsWith("--- ")) continue;
      if (line.startsWith("+") || line.startsWith("-")) {
        currentPreview = line.slice(0, 120);
      }
    }
  }
  flush();
  return out;
}

async function fetchDiff(
  query: string,
  root: string | null,
): Promise<VocabEntry[]> {
  if (!root) return [];
  let diff = "";
  try {
    diff = await gitWorkingDiff(root);
  } catch {
    return [];
  }
  const entries = parseDiffPerFile(diff);
  if (entries.length === 0) {
    return [
      {
        kind: "diff",
        label: "(no working changes)",
        value: envelope("diff", ""),
        preview: "git diff HEAD is empty",
      },
    ];
  }
  return entries
    .filter((e) => fuzzyMatch(e.path, query))
    .slice(0, MAX_PER_KIND)
    .map((e) => ({
      kind: "diff" as const,
      label: e.path,
      value: envelope("diff", e.path),
      preview: e.preview,
    }));
}

async function fetchProblems(
  _query: string,
  root: string | null,
): Promise<VocabEntry[]> {
  if (!root) return [];
  // The composer injects a single bare `@problems` token (resolved server-side
  // by `expand_at_tokens` → `projects::diagnostics::collect`, which inlines the
  // *whole* current error/warning list). Per-diagnostic tokens aren't
  // addressable — diagnostics are an ephemeral snapshot with no stable id — so
  // we surface one actionable entry and use the live count only as a preview.
  let count: number | null = null;
  try {
    count = (await projectDiagnostics(root)).length;
  } catch {
    count = null;
  }
  const preview =
    count === null
      ? "Inject all compiler errors & warnings (cargo check / tsc) as context"
      : count === 0
        ? "No problems right now — cargo check / tsc are clean"
        : `${count} compile error(s)/warning(s) — inject all as context`;
  return [{ kind: "problems" as const, label: "@problems", value: "problems", preview }];
}

async function fetchFragments(query: string): Promise<VocabEntry[]> {
  try {
    const names = await invoke<string[]>("list_fragments").catch(() => []);
    const q = query.toLowerCase();
    return names
      .filter((n) => !q || n.toLowerCase().includes(q))
      .slice(0, MAX_PER_KIND)
      .map((n) => ({ kind: "frag" as const, label: `@frag:${n}`, value: `frag:${n}`, preview: `~/.cortex/fragments/${n}.md` }));
  } catch {
    return [];
  }
}

async function fetchTerminal(query: string): Promise<VocabEntry[]> {
  let output: string | null = null;
  try {
    output = await recentTerminalOutput();
  } catch {
    return [];
  }
  if (!output) {
    return [
      {
        kind: "terminal",
        label: "(no terminal output)",
        value: envelope("terminal", "last"),
        preview: "~/.cortex/last-shell-output.log not found",
      },
    ];
  }
  // The picker only renders a single line of preview, so collapse newlines.
  const snippet = output.replace(/\s+/g, " ").trim().slice(0, 160);
  if (query && !fuzzyMatch(snippet, query)) return [];
  return [
    {
      kind: "terminal",
      label: "last terminal output",
      value: envelope("terminal", "last"),
      preview: snippet || "(empty)",
    },
  ];
}

/**
 * Fetch up to 20 vocab entries for a given kind, fuzzy-filtered by `query`.
 * `activeProjectRoot` is required for `files`/`folders`/`memory`/`git`/`diff`/`problems`;
 * `recent`, `docs`, `threads`, `diagnostics`, `snippets` and `terminal` work without it.
 */
export async function fetchVocab(
  kind: VocabKind,
  query: string,
  activeProjectRoot: string | null,
): Promise<VocabEntry[]> {
  switch (kind) {
    case "files":
      return fetchFiles(query, activeProjectRoot, false);
    case "folders":
      return fetchFiles(query, activeProjectRoot, true);
    case "recent":
      return fetchRecent(query);
    case "memory":
      return fetchMemory(query, activeProjectRoot);
    case "docs":
      return fetchDocs(query);
    case "git":
      return fetchGit(query, activeProjectRoot);
    case "symbols":
      return fetchSymbols(query, activeProjectRoot);
    case "threads":
      return fetchThreads(query);
    case "diagnostics":
      return fetchDiagnostics(query);
    case "snippets":
      return fetchSnippetsVocab(query);
    case "diff":
      return fetchDiff(query, activeProjectRoot);
    case "problems":
      return fetchProblems(query, activeProjectRoot);
    case "terminal":
      return fetchTerminal(query);
    case "brain":
      return [{ kind: "brain", label: "@brain", value: "brain", preview: "Auto-attach top 3 brain hits for this message" }];
    case "status":
      return [{ kind: "status", label: "@status", value: "status", preview: "git status --short of active project" }];
    case "recent-edits":
      return [{ kind: "recent-edits", label: "@recent", value: "recent", preview: "Last 8 modified files in active project" }];
    case "frag":
      return await fetchFragments(query);
    case "web":
      return [{ kind: "web", label: "@web:https://…", value: "web:https://", preview: "Type a URL to fetch and inline" }];
    case "websearch":
      return [{ kind: "websearch", label: "@websearch:<query>", value: "websearch:", preview: "Live web search — inline the top results (title · url · snippet)" }];
    case "grep":
      return [{ kind: "grep", label: "@grep:<pattern>", value: "grep:", preview: "Type a search pattern (case-insensitive)" }];
    case "codebase":
      return [{ kind: "codebase", label: "@codebase", value: "codebase", preview: "Semantic retrieval over the project, ranked by your message" }];
    case "cwd":
      return [{ kind: "cwd", label: "@cwd", value: "cwd", preview: "Top-level file tree of active project" }];
    case "tree":
      return [{ kind: "tree", label: "@tree", value: "tree", preview: "Directory tree of active project (depth 2; @tree:N tunes 1–6)" }];
    case "outline":
      return [{ kind: "outline", label: "@outline:<file>", value: "outline:", preview: "Symbol outline of a file — functions, classes, headings + line numbers" }];
    case "def":
      return [{ kind: "def", label: "@def:<symbol>", value: "def:", preview: "Go to a symbol's definition — its declaration site(s) + body across the project" }];
    case "refs":
      return [{ kind: "refs", label: "@refs:<symbol>", value: "refs:", preview: "Find all references — every use of a symbol across the project (whole-word), with the declaration marked" }];
    case "env":
      return [{ kind: "env", label: "@env", value: "env", preview: "Project root + git HEAD + branch" }];
    default:
      return [];
  }
}
