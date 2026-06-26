import { useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { Camera, Sparkles, Plus, SquarePen, Copy, Play, Search } from "lucide-react";
import { useCortexStore } from "@/state/store";
import {
  getMemoryEntry,
  listMemoryFiles,
  searchMemory,
  type MarkdownEntry,
  type MemoryFile,
  type MemorySearchHit,
} from "@/lib/memory";
import {
  getClaudeChat,
  listClaudeChats,
  searchClaudeChats,
  type ChatSearchHit,
  type ChatSummary,
  type ChatTranscript,
} from "@/lib/chat-history";
import { SnapshotsPanel } from "@/components/SnapshotsPanel";
import { semanticMemorySearch } from "@/lib/semantic-search";

type SourceFilter =
  | "all"
  | "memory"
  | "chats"
  | "obsidian"
  | "runbooks"
  | "claude"
  | "project"
  | "global";

interface UnifiedRow {
  id: string;
  kind: "memory" | "chat";
  title: string;
  subtitle: string;
  preview: string;
  source: string;
  sourceKind: string;
  modified: number;
  path: string;
}

// Order matters: Chats and Obsidian appear immediately after "All" so the
// two highest-signal sources are the most prominent. Lower-priority tabs
// (memory/runbooks/claude/project/global) come after.
const SOURCE_TABS: { key: SourceFilter; label: string; hint: string }[] = [
  { key: "all", label: "All", hint: "Everything" },
  { key: "chats", label: "Chats", hint: "Claude Code history" },
  { key: "obsidian", label: "Obsidian", hint: "Cortex Brain vault" },
  { key: "memory", label: "Memory", hint: "Markdown notes" },
  { key: "runbooks", label: "Runbooks", hint: "Ops knowledge" },
  { key: "claude", label: "Auto-memory", hint: "~/.claude/projects" },
  { key: "project", label: "Project", hint: "CLAUDE.md, AGENTS.md" },
  { key: "global", label: "Global", hint: "~/CLAUDE.md, ~/.codex/AGENTS.md" },
];

function basename(p: string): string {
  const m = p.match(/([^/\\]+)$/);
  return m ? m[1] : p;
}

function sourceMatchesFilter(row: UnifiedRow, filter: SourceFilter): boolean {
  if (filter === "all") return true;
  if (filter === "chats") return row.kind === "chat";
  if (filter === "memory") return row.kind === "memory";
  if (filter === "obsidian") return row.sourceKind === "obsidian";
  if (filter === "runbooks") return row.sourceKind === "runbooks";
  if (filter === "claude") return row.sourceKind === "claude_project_memory";
  if (filter === "project") return row.sourceKind === "project_instructions";
  if (filter === "global") return row.sourceKind === "global_instructions";
  return true;
}

export function MemoryExplorer({ autoFocus = true }: { autoFocus?: boolean } = {}) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [filter, setFilter] = useState<SourceFilter>("all");
  // Whether we've already applied the one-time smart default. We never
  // override an explicit user choice, even if counts change later.
  const [appliedSmartDefault, setAppliedSmartDefault] = useState(false);
  const [query, setQuery] = useState("");
  const [semantic, setSemantic] = useState<boolean>(() => {
    try {
      return localStorage.getItem("cortex.memex.semantic") === "true";
    } catch {
      return false;
    }
  });
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [memory, setMemory] = useState<MemoryFile[]>([]);
  const [chats, setChats] = useState<ChatSummary[]>([]);
  const [memHits, setMemHits] = useState<MemorySearchHit[]>([]);
  const [chatHits, setChatHits] = useState<ChatSearchHit[]>([]);
  const [selected, setSelected] = useState<UnifiedRow | null>(null);
  const [detail, setDetail] = useState<MarkdownEntry | ChatTranscript | null>(null);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showSnapshots, setShowSnapshots] = useState(false);
  // Bumped by the error-state Retry button to re-run the list + search
  // effects after a backend failure (e.g. the app finished starting up).
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    let mounted = true;
    (async () => {
      try {
        const [m, c] = await Promise.all([
          listMemoryFiles(activeProject?.root ?? undefined),
          listClaudeChats(),
        ]);
        if (!mounted) return;
        setMemory(m);
        setChats(c);
        setError(null);
      } catch (e) {
        if (mounted) setError(humanizeError(e));
      }
    })();
    return () => {
      mounted = false;
    };
  }, [activeProject?.root, reloadKey]);

  // Debounce search input by 250ms.
  useEffect(() => {
    const id = setTimeout(() => setDebouncedQuery(query.trim()), 250);
    return () => clearTimeout(id);
  }, [query]);

  // Run the search across both sources when the query settles.
  useEffect(() => {
    let mounted = true;
    if (!debouncedQuery) {
      setMemHits([]);
      setChatHits([]);
      return;
    }
    (async () => {
      try {
        if (semantic) {
          // Semantic mode: rank vault/memory by meaning via Ollama embeddings.
          // Map into the existing memory-hit shape; chats are skipped here.
          const hits = await semanticMemorySearch(
            debouncedQuery,
            activeProject?.root ?? null,
            20,
          );
          if (!mounted) return;
          setMemHits(
            hits.map((h) => ({
              source: h.mode === "semantic" ? "semantic" : "lexical",
              path: h.path,
              snippet: h.snippet,
              score: h.score,
            })),
          );
          setChatHits([]);
          return;
        }
        const [mh, ch] = await Promise.all([
          searchMemory(debouncedQuery, {
            activeProject: activeProject?.root ?? undefined,
            includeChroma: false,
          }),
          searchClaudeChats(debouncedQuery, 30),
        ]);
        if (!mounted) return;
        setMemHits(mh);
        setChatHits(ch);
      } catch (e) {
        if (mounted) setError(humanizeError(e));
      }
    })();
    return () => {
      mounted = false;
    };
  }, [debouncedQuery, activeProject?.root, semantic, reloadKey]);

  // Unified row list: when there's a query, show hits; else show recents.
  const rows: UnifiedRow[] = useMemo(() => {
    if (debouncedQuery) {
      const mRows: UnifiedRow[] = memHits.map((h) => ({
        id: `mem:${h.path}`,
        kind: "memory",
        title: basename(h.path),
        subtitle: h.source,
        preview: h.snippet,
        source: h.source,
        sourceKind: "memory",
        modified: 0,
        path: h.path,
      }));
      const cRows: UnifiedRow[] = chatHits.map((h) => ({
        id: `chat:${h.file_path}:${h.session_id}`,
        kind: "chat",
        title:
          h.project ??
          (h.file_path.includes("history.jsonl")
            ? "global history"
            : h.session_id.slice(-12)),
        subtitle: `${h.role} · ${timeAgo(h.modified_unix_ms)}`,
        preview: h.snippet,
        source: "claude",
        sourceKind: "chats",
        modified: h.modified_unix_ms,
        path: h.file_path,
      }));
      return [...mRows, ...cRows].sort((a, b) => b.modified - a.modified);
    }
    const mRows: UnifiedRow[] = memory.map((m) => ({
      id: `mem:${m.path}`,
      kind: "memory",
      title: m.name,
      subtitle: m.source,
      preview: "",
      source: m.source,
      sourceKind: m.source_kind,
      modified: m.modified_unix_ms,
      path: m.path,
    }));
    const cRows: UnifiedRow[] = chats.map((c) => ({
      id: `chat:${c.file_path}`,
      kind: "chat",
      title: c.first_message ?? c.session_id.slice(-12),
      subtitle: `${c.project ?? "—"} · ${c.message_count} msgs`,
      preview: "",
      source: "claude",
      sourceKind: "chats",
      modified: c.modified_unix_ms,
      path: c.file_path,
    }));
    return [...mRows, ...cRows].sort((a, b) => b.modified - a.modified);
  }, [debouncedQuery, memHits, chatHits, memory, chats]);

  const filtered = useMemo(
    () => rows.filter((r) => sourceMatchesFilter(r, filter)),
    [rows, filter],
  );

  const select = async (row: UnifiedRow) => {
    setSelected(row);
    setDetail(null);
    setLoadingDetail(true);
    try {
      if (row.kind === "chat") {
        // Preview only — don't auto-replace the live chat. The "▶ Resume in
        // chat" button in the detail view is the explicit resume trigger.
        // Previous behaviour silently wiped the active chat on every
        // single click, which made browsing impossible.
        const t = await getClaudeChat(row.path, 200);
        setDetail(t);
      } else {
        const e = await getMemoryEntry(row.path);
        setDetail(e);
      }
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoadingDetail(false);
    }
  };

  // Esc closes the detail modal.
  useEffect(() => {
    if (!selected) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setSelected(null);
        setDetail(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected]);

  const counts = useMemo(() => {
    return {
      all: rows.length,
      memory: rows.filter((r) => r.kind === "memory").length,
      chats: rows.filter((r) => r.kind === "chat").length,
      obsidian: rows.filter((r) => r.sourceKind === "obsidian").length,
      runbooks: rows.filter((r) => r.sourceKind === "runbooks").length,
      claude: rows.filter((r) => r.sourceKind === "claude_project_memory").length,
      project: rows.filter((r) => r.sourceKind === "project_instructions").length,
      global: rows.filter((r) => r.sourceKind === "global_instructions").length,
    };
  }, [rows]);

  // Smart default — runs once after the first listMemoryFiles + listClaudeChats
  // settle. Rule: if there are any chats, default to "all" (chats are now
  // first in the tab strip so they're already visible). Otherwise pick the
  // non-"all" tab with the most entries so the panel never looks empty.
  useEffect(() => {
    if (appliedSmartDefault) return;
    if (rows.length === 0) return; // wait for data
    setAppliedSmartDefault(true);
    if (counts.chats > 0) {
      // "all" already gives chats top-of-list because of source ordering.
      setFilter("all");
      return;
    }
    const candidates: { key: SourceFilter; n: number }[] = [
      { key: "obsidian", n: counts.obsidian },
      { key: "runbooks", n: counts.runbooks },
      { key: "memory", n: counts.memory },
      { key: "claude", n: counts.claude },
      { key: "project", n: counts.project },
      { key: "global", n: counts.global },
    ];
    candidates.sort((a, b) => b.n - a.n);
    if (candidates[0] && candidates[0].n > 0) {
      setFilter(candidates[0].key);
    }
  }, [appliedSmartDefault, rows.length, counts]);

  return (
    <div className="memex">
      <div className="memex-search">
        <div className="memex-search-field">
          <Search className="memex-search-icon" size={14} strokeWidth={1.75} aria-hidden="true" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search memory, runbooks, and previous Claude chats…"
            autoFocus={autoFocus}
          />
          {query && (
            <button className="link-btn memex-clear" onClick={() => setQuery("")}>
              Clear
            </button>
          )}
        </div>
        <button
          className="link-btn"
          title={semantic ? "Semantic search (Ollama embeddings) — on" : "Switch to semantic search (rank by meaning)"}
          aria-pressed={semantic}
          style={{ color: semantic ? "var(--accent)" : undefined }}
          onClick={() => {
            setSemantic((s) => {
              const next = !s;
              try {
                localStorage.setItem("cortex.memex.semantic", String(next));
              } catch {
                /* ignore */
              }
              return next;
            });
          }}
        >
          <Sparkles size={14} strokeWidth={1.75} aria-hidden="true" /> {semantic ? "Semantic" : "Lexical"}
        </button>
        <button
          className="link-btn memex-snapshots-btn"
          onClick={() => setShowSnapshots(true)}
          title="Memory snapshots & rollback"
        >
          <Camera size={14} strokeWidth={1.75} aria-hidden="true" /> Snapshots
        </button>
      </div>
      {showSnapshots && <SnapshotsPanel onClose={() => setShowSnapshots(false)} />}
      {/* Only show the source-filter chips when there's content to filter (or
          the load succeeded). A hard load error must not stack a strip of
          zero-count filters above the error box — that implies browsable
          content next to a "backend isn't responding" message. */}
      {(!error || rows.length > 0) && (
        <div className="memex-tabs memex-tabs-sticky">
          {SOURCE_TABS.map((t) => (
            <button
              key={t.key}
              className={`memex-tab ${filter === t.key ? "active" : ""}`}
              onClick={() => setFilter(t.key)}
              title={t.hint}
            >
              {t.label}
              <span className="badge">
                {(counts as Record<SourceFilter, number>)[t.key]}
              </span>
            </button>
          ))}
        </div>
      )}
      {rows.length > 0 && (
        <div className="memex-count" title="Filter is hiding entries">
          Showing {filtered.length} of {rows.length}
          {filter !== "all" && (
            <button
              className="link-btn memex-count-reset"
              onClick={() => setFilter("all")}
            >
              Show all
            </button>
          )}
        </div>
      )}
      {/* With content on screen a failure (e.g. one search call) is a compact
          inline strip; with NOTHING loaded it becomes the centered error card
          below instead — never a bare banner over a blank pane (spec §7). */}
      {error && rows.length > 0 && (
        <div className="memex-error" role="alert">
          {error}
        </div>
      )}
      <div className="memex-body">
        <div className="memex-list">
          {filtered.length === 0 && !error && (
            <div className="memex-empty">
              {debouncedQuery
                ? `No matches for "${debouncedQuery}".`
                : "No items in this view yet."}
            </div>
          )}
          {rows.length === 0 && error && (
            <div className="memex-error-state" role="alert">
              <div className="memex-error-state-title">
                Memory is unavailable
              </div>
              <p className="memex-error-state-hint">{error}</p>
              <button
                className="link-btn"
                onClick={() => setReloadKey((k) => k + 1)}
              >
                Retry
              </button>
            </div>
          )}
          {filtered.slice(0, 200).map((row) => (
            <button
              key={row.id}
              className={`brain-row memex-row ${
                selected?.id === row.id ? "active" : ""
              }`}
              onClick={() => select(row)}
            >
              <div className="brain-row-head">
                <strong>{row.title}</strong>
                <span className="muted">{timeAgo(row.modified)}</span>
              </div>
              <div className="brain-meta">
                <span className={`memex-kind kind-${row.kind}`}>{row.kind}</span>
                {" · "}
                {row.subtitle}
              </div>
              {row.preview && <div className="brain-preview">{row.preview}</div>}
            </button>
          ))}
        </div>
      </div>
      <div
        className={`memex-detail ${selected ? "open" : ""}`}
        onClick={(e) => {
          if (e.target === e.currentTarget) {
            setSelected(null);
            setDetail(null);
          }
        }}
      >
        {selected && (
          <div className="memex-detail-card">
            <button
              className="memex-detail-close"
              onClick={() => {
                setSelected(null);
                setDetail(null);
              }}
              title="Close (Esc)"
            >
              ×
            </button>
            {loadingDetail && <div className="muted">loading…</div>}
            {!loadingDetail && detail && <DetailView row={selected} detail={detail} />}
          </div>
        )}
      </div>
    </div>
  );
}

function DetailView({
  row,
  detail,
}: {
  row: UnifiedRow;
  detail: MarkdownEntry | ChatTranscript;
}) {
  if (row.kind === "memory") {
    const m = detail as MarkdownEntry;
    const openInEditor = () => {
      // CRITICAL: switch to the editor tab BEFORE dispatching the open event.
      // ActivityPanel only mounts `<EditorPane />` when `activityTab === "editor"`,
      // and EditorPane is the one that listens for `cortex:editor-open`. If we
      // dispatch first, the listener doesn't exist yet and the event vanishes.
      useCortexStore.getState().setActivityTab("editor");
      setTimeout(() => {
        try {
          window.dispatchEvent(
            new CustomEvent("cortex:editor-open", { detail: { path: m.path } }),
          );
        } catch { /* non-fatal */ }
      }, 0);
    };
    const copyContent = () => {
      void navigator.clipboard.writeText(m.body);
    };
    // Add this file as @-context in the chat composer. ChatPane subscribes
    // to `cortex:composer-insert` (added wave 55) and splices the token
    // at the cursor, ready to send as a task that references this memory.
    const addToChat = () => {
      const token = `@${m.path}`;
      window.dispatchEvent(
        new CustomEvent("cortex:composer-insert", { detail: { value: token } }),
      );
    };
    return (
      <div className="memex-detail-body">
        <div className="memex-detail-head">
          <strong>{m.title ?? basename(m.path)}</strong>
          <span className="muted">{m.path}</span>
        </div>
        <div className="memex-detail-actions">
          <button className="btn-primary" onClick={addToChat}>
            <Plus size={14} strokeWidth={1.75} aria-hidden="true" /> Add to chat
          </button>
          <button onClick={openInEditor}>
            <SquarePen size={14} strokeWidth={1.75} aria-hidden="true" /> Edit in editor
          </button>
          <button onClick={copyContent}>
            <Copy size={14} strokeWidth={1.75} aria-hidden="true" /> Copy
          </button>
        </div>
        <pre className="memex-md">{m.body.slice(0, 8000)}</pre>
      </div>
    );
  }
  const t = detail as ChatTranscript;
  const resumeInChat = () => {
    const firstUser = t.turns.find((x) => x.role === "user");
    try {
      // Forward `file_path` so the ChatPane listener can read the actual
      // Claude `.jsonl` instead of the empty SQLite messages table.
      window.dispatchEvent(
        new CustomEvent("cortex:chat-replay", {
          detail: {
            content: firstUser?.content ?? "",
            session_id: t.session_id,
            file_path: t.file_path,
          },
        }),
      );
    } catch { /* non-fatal */ }
  };
  const copyTranscript = () => {
    const md = t.turns
      .map((turn) => `### ${turn.role}\n\n${turn.content}\n`)
      .join("\n");
    void navigator.clipboard.writeText(md);
  };
  return (
    <div className="memex-detail-body">
      <div className="memex-detail-head">
        <strong>session {t.session_id.slice(-10)}</strong>
        <span className="muted">
          {t.turns.length} turns · {t.project_root ?? "—"}
        </span>
      </div>
      <div className="memex-detail-actions">
        <button className="btn-primary" onClick={resumeInChat}>
          <Play size={14} strokeWidth={1.75} aria-hidden="true" /> Resume in chat
        </button>
        <button onClick={copyTranscript}>
          <Copy size={14} strokeWidth={1.75} aria-hidden="true" /> Copy as markdown
        </button>
      </div>
      <div className="memex-turns">
        {t.turns.map((turn, i) => (
          <div key={i} className={`memex-turn turn-${turn.role}`}>
            <div className="memex-turn-role">{turn.role}</div>
            <div className="memex-turn-body">{turn.content}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
