import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import { useCortexStore } from "@/state/store";
import {
  findFiles,
  groupHitsByFile,
  searchProject,
  shortenPath,
  type SearchHit,
} from "@/lib/project-search";
import { searchMemory, type MemorySearchHit } from "@/lib/memory";
import { openInEditor } from "@/lib/editor";

/**
 * Unified project + memory search ("search universes").
 *
 * One entry point searches across BOTH search universes and groups the
 * results under clearly-labelled sections:
 *   - "Project"      — text/regex matches inside the active project's files
 *                      (via `search_project`), and fuzzy file-path matches
 *                      (via `find_files`, in the "Go to file" scope).
 *   - "Memory / Vault" — markdown notes, runbooks, and the Obsidian vault
 *                      (via `search_memory`). Chats live in the Memory
 *                      explorer; this surface stays file-oriented.
 *
 * A scope selector lets you narrow to one universe, but the default
 * ("Everything") fans out to both so a single query reaches the whole
 * workspace. Results navigate to the right place when clicked: project +
 * memory files open in the inline editor; the "Go to file" scope is a
 * project-only fuzzy path picker.
 *
 * Wiring:
 *   - mounted from `ActivityPanel.tsx` when `activityTab === "search"`
 *   - opened by the `/search [query]` slash command (see slash-commands.ts)
 */

/**
 * Search scope.
 *   - "all"    — fan out across project files + memory/vault (default)
 *   - "text"   — project full-text search only
 *   - "memory" — memory/vault search only
 *   - "files"  — project fuzzy path picker ("Go to file")
 */
type Mode = "all" | "text" | "memory" | "files";

interface SearchPanelHandle {
  preload?: string;
  mode?: Mode;
}

let preloaded: SearchPanelHandle = {};

/** Pre-seed the panel's query before/while it mounts. Called by `/search`. */
export function setSearchPreload(handle: SearchPanelHandle): void {
  preloaded = { ...handle };
}

const SCOPES: { key: Mode; label: string; title: string }[] = [
  { key: "all", label: "Everything", title: "Search project files and memory/vault" },
  { key: "text", label: "Project", title: "Find in project files" },
  { key: "memory", label: "Memory", title: "Search memory, runbooks & vault" },
  { key: "files", label: "Go to file", title: "Fuzzy file-path search" },
];

/** Last path segment, for memory-hit titles. */
function basename(p: string): string {
  const m = p.match(/([^/\\]+)$/);
  return m ? m[1] : p;
}

export function SearchPanel() {
  const project = useCortexStore((s) => s.activeProject);
  const [mode, setMode] = useState<Mode>(preloaded.mode ?? "all");
  const [query, setQuery] = useState<string>(preloaded.preload ?? "");
  const [debounced, setDebounced] = useState<string>(query.trim());
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [fixedString, setFixedString] = useState(false);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [memHits, setMemHits] = useState<MemorySearchHit[]>([]);
  const [files, setFiles] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Clear the preload once we've consumed it so a later remount doesn't
  // resurrect a stale query.
  useEffect(() => {
    preloaded = {};
    inputRef.current?.focus();
  }, []);

  // 250ms debounce — matches VS Code's "Search" panel.
  useEffect(() => {
    const id = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(id);
  }, [query]);

  const wantsProjectText = mode === "all" || mode === "text";
  const wantsMemory = mode === "all" || mode === "memory";
  const wantsFiles = mode === "files";

  useEffect(() => {
    let cancelled = false;
    setError(null);

    // "Go to file" is project-only and lists files even with an empty query.
    if (wantsFiles) {
      setHits([]);
      setMemHits([]);
      if (!project) {
        setFiles([]);
        return;
      }
      setLoading(true);
      findFiles(project.root, debounced)
        .then((r) => {
          if (!cancelled) setFiles(r);
        })
        .catch((e) => {
          if (!cancelled) {
            setFiles([]);
            setError(humanizeError(e));
          }
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
      return () => {
        cancelled = true;
      };
    }

    setFiles([]);

    // The other scopes are query-driven: an empty query shows the hint.
    if (!debounced) {
      setHits([]);
      setMemHits([]);
      setLoading(false);
      return;
    }

    setLoading(true);
    const tasks: Promise<void>[] = [];
    let sawError: unknown = null;

    if (wantsProjectText && project) {
      tasks.push(
        searchProject(project.root, debounced, { caseSensitive, fixedString })
          .then((r) => {
            if (!cancelled) setHits(r);
          })
          .catch((e) => {
            sawError = e;
            if (!cancelled) setHits([]);
          }),
      );
    } else {
      setHits([]);
    }

    if (wantsMemory) {
      tasks.push(
        searchMemory(debounced, {
          activeProject: project?.root ?? undefined,
          includeChroma: false,
        })
          .then((r) => {
            if (!cancelled) setMemHits(r);
          })
          .catch((e) => {
            sawError = e;
            if (!cancelled) setMemHits([]);
          }),
      );
    } else {
      setMemHits([]);
    }

    Promise.all(tasks).finally(() => {
      if (cancelled) return;
      setLoading(false);
      // Only surface an error when nothing came back — a partial failure of
      // one universe shouldn't blank out the other's results.
      if (sawError) setError(humanizeError(sawError));
    });

    return () => {
      cancelled = true;
    };
  }, [
    project,
    mode,
    debounced,
    caseSensitive,
    fixedString,
    wantsProjectText,
    wantsMemory,
    wantsFiles,
  ]);

  // Project-scoped modes need an active project; memory search is global.
  const needsProject = mode === "text" || mode === "files";
  if (needsProject && !project) {
    return (
      <div className="search-panel">
        <ScopeBar mode={mode} onChange={setMode} />
        <div className="search-empty">
          <div className="search-empty-title">No active project</div>
          <p className="search-empty-hint">
            Pick a project from the sidebar to search its files, or switch to{" "}
            <button className="link-btn" onClick={() => setMode("memory")}>
              Memory
            </button>{" "}
            to search your vault.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="search-panel">
      <ScopeBar mode={mode} onChange={setMode} />
      <div className="search-panel-head">
        <input
          ref={inputRef}
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={placeholderFor(mode)}
          className="search-panel-input"
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
        />
        {wantsProjectText && (
          <div className="search-panel-toggles">
            <label className="search-toggle" title="Match case (Aa)">
              <input
                type="checkbox"
                checked={caseSensitive}
                onChange={(e) => setCaseSensitive(e.target.checked)}
              />
              <span>Aa</span>
            </label>
            <label className="search-toggle" title="Match as literal string (no regex)">
              <input
                type="checkbox"
                checked={fixedString}
                onChange={(e) => setFixedString(e.target.checked)}
              />
              <span>&quot;abc&quot;</span>
            </label>
          </div>
        )}
      </div>
      <div className="search-panel-body">
        {error && <div className="search-panel-error">{error}</div>}
        {loading && !error && <PanelLoading label="Searching" />}
        {!loading && !error && wantsFiles && (
          <FileHits files={files} projectRoot={project?.root ?? null} />
        )}
        {!loading && !error && !wantsFiles && (
          <UnifiedResults
            query={debounced}
            hits={wantsProjectText ? hits : []}
            memHits={wantsMemory ? memHits : []}
            projectRoot={project?.root ?? null}
            showProject={wantsProjectText && !!project}
            showMemory={wantsMemory}
          />
        )}
      </div>
    </div>
  );
}

function placeholderFor(mode: Mode): string {
  switch (mode) {
    case "all":
      return "Search project + memory…";
    case "text":
      return "Search in project…";
    case "memory":
      return "Search memory, runbooks & vault…";
    case "files":
      return "File path…";
  }
}

function ScopeBar({ mode, onChange }: { mode: Mode; onChange: (m: Mode) => void }) {
  return (
    <div className="search-scopes" role="tablist" aria-label="Search scope">
      {SCOPES.map((s) => (
        <button
          key={s.key}
          role="tab"
          aria-selected={mode === s.key}
          className={`search-scope-btn ${mode === s.key ? "active" : ""}`}
          onClick={() => onChange(s.key)}
          title={s.title}
        >
          {s.label}
        </button>
      ))}
    </div>
  );
}

/**
 * Renders both universes as labelled sections. Each universe owns its own
 * empty state so a query that matches files but no notes (or vice-versa)
 * still reads clearly.
 */
function UnifiedResults({
  query,
  hits,
  memHits,
  projectRoot,
  showProject,
  showMemory,
}: {
  query: string;
  hits: SearchHit[];
  memHits: MemorySearchHit[];
  projectRoot: string | null;
  showProject: boolean;
  showMemory: boolean;
}) {
  const grouped = useMemo(() => groupHitsByFile(hits), [hits]);

  if (!query) {
    return (
      <div className="search-empty">
        <div className="search-empty-title">Search your workspace</div>
        <p className="search-empty-hint">
          Type to search across{" "}
          {showProject && showMemory
            ? "project files and your memory/vault"
            : showProject
              ? "files in this project"
              : "memory, runbooks & vault"}
          .
        </p>
      </div>
    );
  }

  const projectCount = grouped.reduce((n, g) => n + g.hits.length, 0);
  const memoryCount = memHits.length;
  const totalSections = (showProject ? 1 : 0) + (showMemory ? 1 : 0);

  if (projectCount === 0 && memoryCount === 0) {
    return (
      <div className="search-empty">
        <div className="search-empty-title">No matches</div>
        <p className="search-empty-hint">
          Nothing for &ldquo;{query}&rdquo;
          {totalSections > 1 ? " in either universe" : ""}. Try a different
          term or widen the scope.
        </p>
      </div>
    );
  }

  return (
    <div className="search-results">
      {showProject && (
        <SearchSection label="Project" count={projectCount}>
          {projectCount === 0 ? (
            <div className="search-section-empty">No file matches.</div>
          ) : (
            grouped.map((group) => (
              <div key={group.path} className="search-group">
                <div className="search-group-head" title={group.path}>
                  {shortenPath(group.path, projectRoot)}
                  <span className="muted"> ({group.hits.length})</span>
                </div>
                {group.hits.map((h, i) => (
                  <button
                    key={`${h.path}-${h.line}-${h.col}-${i}`}
                    className="search-hit"
                    onClick={() => openInEditor(h.path)}
                    title={`${h.path}:${h.line}:${h.col}`}
                  >
                    <span className="search-hit-line">{h.line}</span>
                    <span className="search-hit-text">
                      {h.before && (
                        <span className="search-hit-context">{h.before + "\n"}</span>
                      )}
                      <Highlighted text={h.match_text} query={query} />
                      {h.after && (
                        <span className="search-hit-context">{"\n" + h.after}</span>
                      )}
                    </span>
                  </button>
                ))}
              </div>
            ))
          )}
        </SearchSection>
      )}
      {showMemory && (
        <SearchSection label="Memory / Vault" count={memoryCount}>
          {memoryCount === 0 ? (
            <div className="search-section-empty">No notes or vault matches.</div>
          ) : (
            memHits.map((h, i) => (
              <button
                key={`${h.path}-${i}`}
                className="search-mem-hit"
                onClick={() => openInEditor(h.path)}
                title={h.path}
              >
                <div className="search-mem-hit-head">
                  <span className="search-mem-title">{basename(h.path)}</span>
                  <span className="search-mem-source">{h.source}</span>
                </div>
                {h.snippet && (
                  <div className="search-mem-snippet">
                    <Highlighted text={h.snippet} query={query} />
                  </div>
                )}
              </button>
            ))
          )}
        </SearchSection>
      )}
    </div>
  );
}

function SearchSection({
  label,
  count,
  children,
}: {
  label: string;
  count: number;
  children: ReactNode;
}) {
  return (
    <div className="search-section">
      <div className="search-section-head">
        <span className="search-section-label">{label}</span>
        <span className="search-section-count">{count}</span>
      </div>
      {children}
    </div>
  );
}

function FileHits({ files, projectRoot }: { files: string[]; projectRoot: string | null }) {
  if (files.length === 0) {
    return (
      <div className="search-empty">
        <div className="search-empty-title">No files</div>
        <p className="search-empty-hint">
          Nothing matched. Try a shorter or different path fragment.
        </p>
      </div>
    );
  }
  return (
    <div className="search-results">
      {files.map((p) => (
        <button
          key={p}
          className="search-file-row"
          onClick={() => openInEditor(p)}
          title={p}
        >
          {shortenPath(p, projectRoot)}
        </button>
      ))}
    </div>
  );
}

function Highlighted({ text, query }: { text: string; query: string }) {
  const parts = useMemo(() => splitHighlight(text, query), [text, query]);
  return (
    <>
      {parts.map((p, i) =>
        p.match ? <mark key={i}>{p.text}</mark> : <span key={i}>{p.text}</span>,
      )}
    </>
  );
}

function splitHighlight(text: string, query: string): { text: string; match: boolean }[] {
  if (!query) return [{ text, match: false }];
  const out: { text: string; match: boolean }[] = [];
  const lcText = text.toLowerCase();
  const lcQuery = query.toLowerCase();
  let i = 0;
  while (i < text.length) {
    const j = lcText.indexOf(lcQuery, i);
    if (j === -1) {
      out.push({ text: text.slice(i), match: false });
      break;
    }
    if (j > i) out.push({ text: text.slice(i, j), match: false });
    out.push({ text: text.slice(j, j + lcQuery.length), match: true });
    i = j + lcQuery.length;
  }
  return out;
}
