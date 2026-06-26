import { useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Search } from "lucide-react";
import { OnboardingTour } from "./OnboardingTour";
import { triggerTour } from "@/lib/onboarding";
import { COMMANDS, categorize, CATEGORY_ORDER } from "@/lib/slash-commands";
import { AT_PROVIDERS, AT_PROVIDER_CATEGORIES, type AtProvider } from "@/lib/at-vocab";

/**
 * Help panel — a flat reference of every major Cortex feature, grouped by
 * category and rendered as collapsible sections. Mounted from ActivityPanel
 * when `activityTab === "help"`. It also self-mounts the `OnboardingTour`
 * portal so we don't need to wire it into App.tsx.
 *
 * Two sections are generated *live* from their real registries rather than
 * hand-curated subsets, so they can never drift from what's actually wired:
 *   • "Slash commands" → `COMMANDS`
 *   • "@-mentions"     → `AT_PROVIDERS`
 * (`@-mentions` replaced two stale hand-written lists that, between them, still
 * referenced a nonexistent "LSP index" and omitted the fully-shipped @codebase
 * provider — the precise drift this live-rendering kills.)
 */

interface Section {
  id: string;
  title: string;
  items: string[];
  hint?: string;
  /** When set, the body renders a generated reference instead of `items`. */
  dynamic?: "commands" | "atvocab";
}

/** One command, flattened for the live reference. */
interface CmdEntry {
  name: string;
  usage?: string;
  description: string;
  aliases?: string[];
}

/** One category bucket of commands, in `CATEGORY_ORDER`. */
interface CmdGroup {
  category: string;
  cmds: CmdEntry[];
}

/**
 * Group the live `COMMANDS` registry by `categorize()` into `CATEGORY_ORDER`,
 * de-duping by canonical name (a command may be pushed more than once during
 * hot-reload) and alpha-sorting within each bucket. Pure — safe inside useMemo.
 */
function buildCommandGroups(): CmdGroup[] {
  const byCat = new Map<string, CmdEntry[]>();
  const seen = new Set<string>();
  for (const c of COMMANDS) {
    if (seen.has(c.name)) continue;
    seen.add(c.name);
    const cat = categorize(c.name);
    const arr = byCat.get(cat) ?? [];
    arr.push({ name: c.name, usage: c.usage, description: c.description, aliases: c.aliases });
    byCat.set(cat, arr);
  }
  return CATEGORY_ORDER.filter((cat) => byCat.has(cat)).map((cat) => ({
    category: cat,
    cmds: (byCat.get(cat) as CmdEntry[]).sort((a, b) => a.name.localeCompare(b.name)),
  }));
}

/** Does a command match the (already-lowercased) filter query? */
function cmdMatches(c: CmdEntry, q: string): boolean {
  if (!q) return true;
  if (c.name.toLowerCase().includes(q)) return true;
  if (c.description.toLowerCase().includes(q)) return true;
  return (c.aliases ?? []).some((a) => a.toLowerCase().includes(q));
}

/** One category bucket of `@`-providers, in `AT_PROVIDER_CATEGORIES` order. */
interface AtGroup {
  category: string;
  providers: AtProvider[];
}

/**
 * Group the live `AT_PROVIDERS` registry into `AT_PROVIDER_CATEGORIES`, keeping
 * each category's declared order. Pure — safe inside useMemo.
 */
function buildAtGroups(): AtGroup[] {
  return AT_PROVIDER_CATEGORIES.map((category) => ({
    category,
    providers: AT_PROVIDERS.filter((p) => p.category === category),
  })).filter((g) => g.providers.length > 0);
}

/** Does an `@`-provider match the (already-lowercased) filter query? */
function atMatches(p: AtProvider, q: string): boolean {
  if (!q) return true;
  if (p.syntax.toLowerCase().includes(q)) return true;
  if (p.summary.toLowerCase().includes(q)) return true;
  return (p.aliases ?? []).some((a) => a.toLowerCase().includes(q));
}

const SECTIONS: Section[] = [
  {
    id: "chat",
    title: "Chat",
    hint: "Ctrl+Enter to send",
    items: [
      "Streaming responses with stop button (Esc cancels in-flight runs)",
      "Tool call cards with risk badges and approval prompts",
      "Plan cards — agent emits a plan before executing",
      "Image attachments via drag-drop on the composer",
      "Snippets — saved prompt templates, insertable from the composer",
      "/web <url> fetches and injects pages as markdown",
    ],
  },
  {
    id: "memory",
    title: "Memory",
    hint: "Ctrl+Shift+F to search",
    items: [
      "8 source filters: notes, sessions, files, tools, web, snippets, threads, custom",
      "Top-bar search across all memory sources at once",
      "Obsidian vault auto-detection on first launch",
      "Chat history scrubber — resume any past Claude conversation",
    ],
  },
  {
    id: "editor",
    title: "Editor",
    hint: "Ctrl+S saves",
    items: [
      "CodeMirror 6 with language auto-detection",
      "Inline ghost-text completions powered by the active model",
      "File explorer sidebar with project-root scoping",
      "Save persists straight to disk via the Tauri filesystem",
    ],
  },
  {
    id: "git",
    title: "Git",
    items: [
      "History panel with DAG visualisation of branches",
      "Source control panel — stage, commit, diff, push, pull",
      "AI commit-message suggester (/commit-msg)",
      "Side-by-side diff viewer for staged and unstaged hunks",
    ],
  },
  {
    id: "agents",
    title: "Agents",
    items: [
      "Roles panel — apply pre-built personas to any agent",
      "Focus chain — agent-managed live to-do list per session",
      "Orchestrator — multi-agent teams with a manager/worker split",
      "Profiles v2 — bundle a model, role, and tool set as a named profile",
    ],
  },
  {
    id: "sandbox",
    title: "Sandbox & Approvals",
    items: [
      "Trust matrix — granular auto-approve toggles per agent×tool",
      "Auto-approve allowlist for low-risk reads",
      "PLAN mode blocks all write/edit tools (Ctrl+M to toggle)",
      "Per-hunk diff review for risky edits before they touch disk",
    ],
  },
  {
    id: "slash",
    title: "Slash commands",
    hint: "Type / in the composer",
    dynamic: "commands",
    items: [],
  },
  {
    id: "atvocab",
    title: "@-mentions",
    hint: "Type @ in the composer",
    dynamic: "atvocab",
    items: [],
  },
  {
    id: "shortcuts",
    title: "Shortcuts",
    items: [
      "Ctrl+K — command palette",
      "Ctrl+Enter — send message",
      "Ctrl+Shift+F — focus memory search",
      "Ctrl+M — toggle ACT/PLAN",
      "Alt+B/D/R/S/W/C/E/G/M — quick-insert @brain/@diff/@recent/@status/@web:/@cwd/@env/@grep:/@repomap (@repomap now PageRank-aware + personalized)",
      "Alt+Shift+S — drop /summarize into composer (Enter to run)",
      "Alt+Shift+R — drop /repomap-top into composer (Enter to run)",
      "Esc — cancel current run / close modal",
    ],
  },
  {
    id: "chatgpt-import",
    title: "ChatGPT import",
    hint: "Type /import-chatgpt",
    items: [
      "Open the Chats sidebar (right rail → Chats tab) and click + ChatGPT",
      "Or run /import-chatgpt to pick the file from anywhere",
      "Point at the `conversations.json` file from your ChatGPT export bundle",
      "Each conversation lands as a `.jsonl` under ~/.claude/projects/chatgpt-import/",
      "Re-imports are idempotent — existing files are skipped, never overwritten",
      "Replay-resume works the same way as Claude Code sessions",
    ],
  },
];

export function HelpPanel() {
  // Default: first section open, rest collapsed.
  const [open, setOpen] = useState<Record<string, boolean>>(() => ({
    [SECTIONS[0].id]: true,
  }));
  const [cmdFilter, setCmdFilter] = useState("");
  const [atFilter, setAtFilter] = useState("");

  // Live command reference, computed once from the real registry.
  const commandGroups = useMemo(() => buildCommandGroups(), []);
  const commandCount = useMemo(
    () => commandGroups.reduce((n, g) => n + g.cmds.length, 0),
    [commandGroups],
  );

  // Live @-mention reference, computed once from the real provider registry.
  const atGroups = useMemo(() => buildAtGroups(), []);
  const atCount = useMemo(
    () => atGroups.reduce((n, g) => n + g.providers.length, 0),
    [atGroups],
  );

  function toggle(id: string) {
    setOpen((prev) => ({ ...prev, [id]: !prev[id] }));
  }

  const q = cmdFilter.trim().toLowerCase();
  const filteredGroups = q
    ? commandGroups
        .map((g) => ({ ...g, cmds: g.cmds.filter((c) => cmdMatches(c, q)) }))
        .filter((g) => g.cmds.length > 0)
    : commandGroups;
  const filteredCount = filteredGroups.reduce((n, g) => n + g.cmds.length, 0);

  const aq = atFilter.trim().toLowerCase();
  const filteredAtGroups = aq
    ? atGroups
        .map((g) => ({ ...g, providers: g.providers.filter((p) => atMatches(p, aq)) }))
        .filter((g) => g.providers.length > 0)
    : atGroups;
  const filteredAtCount = filteredAtGroups.reduce((n, g) => n + g.providers.length, 0);

  return (
    <div className="help-panel">
      <div className="help-panel-head">
        <div className="help-panel-intro">
          A reference of every major Cortex feature. Click a section to expand.
        </div>
        <button
          type="button"
          className="help-tour-btn"
          onClick={() => triggerTour()}
          title="Restart the 5-step feature tour"
        >
          Replay tour
        </button>
      </div>
      <div className="help-sections">
        {SECTIONS.map((s) => {
          const isOpen = !!open[s.id];
          return (
            <section key={s.id} className="help-section">
              <button
                type="button"
                className="help-section-head"
                aria-expanded={isOpen}
                onClick={() => toggle(s.id)}
              >
                <span className="help-section-caret" aria-hidden="true">
                  {isOpen ? <ChevronDown size={14} strokeWidth={1.75} /> : <ChevronRight size={14} strokeWidth={1.75} />}
                </span>
                <span className="help-section-title">{s.title}</span>
                {s.hint && (
                  <span className="help-section-hint">
                    {s.dynamic === "commands"
                      ? `${s.hint} · ${commandCount} commands`
                      : s.dynamic === "atvocab"
                        ? `${s.hint} · ${atCount} providers`
                        : s.hint}
                  </span>
                )}
              </button>
              {isOpen &&
                (s.dynamic === "atvocab" ? (
                  <div className="help-cmd-body help-at-body">
                    <div className="help-cmd-search">
                      <Search size={13} strokeWidth={1.75} aria-hidden="true" />
                      <input
                        type="text"
                        className="help-cmd-filter"
                        placeholder="Filter @-mentions…"
                        value={atFilter}
                        onChange={(e) => setAtFilter(e.target.value)}
                        spellCheck={false}
                        autoComplete="off"
                      />
                    </div>
                    {filteredAtCount === 0 ? (
                      <div className="help-cmd-empty">No @-mentions match “{atFilter.trim()}”.</div>
                    ) : (
                      filteredAtGroups.map((g) => (
                        <div key={g.category} className="help-cmd-group">
                          <div className="help-cmd-cat">{g.category}</div>
                          {g.providers.map((p) => (
                            <div key={p.syntax} className="help-cmd-row">
                              <div className="help-cmd-sig">
                                <code className="help-cmd-name">{p.syntax}</code>
                                {p.aliases && p.aliases.length > 0 && (
                                  <span className="help-cmd-alias">{p.aliases.join(", ")}</span>
                                )}
                              </div>
                              <div className="help-cmd-desc">{p.summary}</div>
                            </div>
                          ))}
                        </div>
                      ))
                    )}
                  </div>
                ) : s.dynamic === "commands" ? (
                  <div className="help-cmd-body">
                    <div className="help-cmd-search">
                      <Search size={13} strokeWidth={1.75} aria-hidden="true" />
                      <input
                        type="text"
                        className="help-cmd-filter"
                        placeholder="Filter commands…"
                        value={cmdFilter}
                        onChange={(e) => setCmdFilter(e.target.value)}
                        spellCheck={false}
                        autoComplete="off"
                      />
                    </div>
                    {filteredCount === 0 ? (
                      <div className="help-cmd-empty">No commands match “{cmdFilter.trim()}”.</div>
                    ) : (
                      filteredGroups.map((g) => (
                        <div key={g.category} className="help-cmd-group">
                          <div className="help-cmd-cat">{g.category}</div>
                          {g.cmds.map((c) => (
                            <div key={c.name} className="help-cmd-row">
                              <div className="help-cmd-sig">
                                <code className="help-cmd-name">/{c.name}</code>
                                {c.usage && <span className="help-cmd-usage">{c.usage}</span>}
                                {c.aliases && c.aliases.length > 0 && (
                                  <span className="help-cmd-alias">
                                    {c.aliases.map((a) => `/${a}`).join(", ")}
                                  </span>
                                )}
                              </div>
                              <div className="help-cmd-desc">{c.description}</div>
                            </div>
                          ))}
                        </div>
                      ))
                    )}
                  </div>
                ) : (
                  <ul className="help-section-body">
                    {s.items.map((it, i) => (
                      <li key={i}>{it}</li>
                    ))}
                  </ul>
                ))}
            </section>
          );
        })}
      </div>
      {/* Self-mounted so we don't need App.tsx changes. The tour renders a
          portal-style fixed overlay; it returns null when not active. */}
      <OnboardingTour />
    </div>
  );
}
