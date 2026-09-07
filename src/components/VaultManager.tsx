import { useEffect, useMemo, useState } from "react";
import { analyzeVault, type VaultAnalysis, type VaultNote } from "@/lib/vault-analysis";
import { humanizeError } from "@/lib/errors";
import { invoke } from "@tauri-apps/api/core";
import { useCortexStore } from "@/state/store";

type SubView = "overview" | "folders" | "tags" | "orphans" | "broken" | "notes";

function charSize(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

function StatCard({
  label,
  value,
  onClick,
  warn,
}: {
  label: string;
  value: number | string;
  onClick?: () => void;
  warn?: boolean;
}) {
  return (
    <button
      type="button"
      className={`vault-stat${warn ? " warn" : ""}`}
      onClick={onClick}
      disabled={!onClick}
    >
      <span className="vault-stat-val">{value}</span>
      <span className="vault-stat-label">{label}</span>
    </button>
  );
}

function FolderList({ analysis }: { analysis: VaultAnalysis }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const toggle = (p: string) => {
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(p)) {
        n.delete(p);
      } else {
        n.add(p);
      }
      return n;
    });
  };

  const topFolders = analysis.folders.filter((f) => {
    if (f.path === "/") return false;
    return !f.path.includes("/");
  });

  const subFolders = (parent: string) =>
    analysis.folders.filter((f) => {
      if (f.path === "/" || f.path === parent) return false;
      const rel = f.path.startsWith(parent + "/")
        ? f.path.slice(parent.length + 1)
        : null;
      return rel !== null && !rel.includes("/");
    });

  const renderFolder = (f: { path: string; note_count: number; total_count: number }, depth: number) => {
    const subs = subFolders(f.path);
    const isExpanded = expanded.has(f.path);
    return (
      <div key={f.path} style={{ paddingLeft: depth * 16 }}>
        <div
          className="vault-folder-row"
          onClick={() => subs.length > 0 && toggle(f.path)}
        >
          <span className="vault-folder-toggle">
            {subs.length > 0 ? (isExpanded ? "v" : ">") : " "}
          </span>
          <span className="vault-folder-name">
            {f.path.split("/").pop()}
          </span>
          <span className="muted">{f.note_count} direct / {f.total_count} total</span>
        </div>
        {isExpanded && subs.map((s) => renderFolder(s, depth + 1))}
      </div>
    );
  };

  return (
    <div className="vault-list">
      {topFolders.map((f) => renderFolder(f, 0))}
      {topFolders.length === 0 && (
        <div className="muted" style={{ padding: 12 }}>All notes are in the vault root.</div>
      )}
    </div>
  );
}

function TagCloud({ analysis }: { analysis: VaultAnalysis }) {
  const max = Math.max(1, ...analysis.tags.map((t) => t.count));
  return (
    <div className="vault-tag-cloud">
      {analysis.tags.map((t) => {
        const scale = 0.75 + (t.count / max) * 0.5;
        return (
          <span
            key={t.tag}
            className="vault-tag-pill"
            style={{ fontSize: `${scale}em` }}
            title={`${t.count} note${t.count !== 1 ? "s" : ""}`}
          >
            #{t.tag}
            <span className="vault-tag-count">{t.count}</span>
          </span>
        );
      })}
      {analysis.tags.length === 0 && (
        <div className="muted" style={{ padding: 12 }}>
          No tags found. Add tags via YAML frontmatter.
        </div>
      )}
    </div>
  );
}

function NoteList({
  notes,
  label,
  onOpen,
}: {
  notes: VaultNote[];
  label: string;
  onOpen: (path: string) => void;
}) {
  const [sortBy, setSortBy] = useState<"title" | "size" | "links">("title");
  const sorted = useMemo(() => {
    const s = [...notes];
    if (sortBy === "title") s.sort((a, b) => a.title.localeCompare(b.title));
    if (sortBy === "size") s.sort((a, b) => b.size - a.size);
    if (sortBy === "links")
      s.sort((a, b) => b.link_count + b.backlink_count - (a.link_count + a.backlink_count));
    return s;
  }, [notes, sortBy]);

  return (
    <div className="vault-list">
      <div className="vault-list-header">
        <span className="muted">{label} ({notes.length})</span>
        <span className="vault-sort-group">
          {(["title", "size", "links"] as const).map((k) => (
            <button
              key={k}
              type="button"
              className={`link-btn${sortBy === k ? " active" : ""}`}
              onClick={() => setSortBy(k)}
            >
              {k}
            </button>
          ))}
        </span>
      </div>
      <div className="vault-notes-grid">
        {sorted.slice(0, 200).map((n) => (
          <div
            key={n.path}
            className="vault-note-row"
            onClick={() => onOpen(n.path)}
            title={n.path}
          >
            <span className="vault-note-title">{n.title}</span>
            <span className="muted vault-note-meta">
              {n.folder || "/"} &middot; {charSize(n.size)} &middot;{" "}
              {n.link_count}out / {n.backlink_count}in
              {n.tags.length > 0 && ` · ${n.tags.join(", ")}`}
            </span>
          </div>
        ))}
        {sorted.length > 200 && (
          <div className="muted" style={{ padding: 8 }}>
            Showing 200 of {sorted.length} notes
          </div>
        )}
      </div>
    </div>
  );
}

function BrokenLinkList({
  analysis,
  onOpen,
}: {
  analysis: VaultAnalysis;
  onOpen: (path: string) => void;
}) {
  return (
    <div className="vault-list">
      <div className="vault-list-header">
        <span className="muted">Broken wikilinks ({analysis.broken_links.length})</span>
      </div>
      {analysis.broken_links.slice(0, 200).map(([source, target], i) => (
        <div
          key={`${source}-${target}-${i}`}
          className="vault-note-row"
          onClick={() => onOpen(source)}
        >
          <span className="vault-note-title">
            [[{target}]]
          </span>
          <span className="muted vault-note-meta">
            in {source}
          </span>
        </div>
      ))}
      {analysis.broken_links.length === 0 && (
        <div className="muted" style={{ padding: 12 }}>No broken links found.</div>
      )}
    </div>
  );
}

export function VaultManager() {
  const [analysis, setAnalysis] = useState<VaultAnalysis | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [view, setView] = useState<SubView>("overview");
  const [autoSortRunning, setAutoSortRunning] = useState(false);
  const [autoSortResult, setAutoSortResult] = useState<string | null>(null);
  const setTab = useCortexStore((s) => s.setActivityTab);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const a = await analyzeVault();
      setAnalysis(a);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void load();
  }, []);

  function openNote(relPath: string) {
    void invoke("open_in_editor", { path: relPath }).catch(() => {});
  }

  const orphans = useMemo(
    () => (analysis ? analysis.notes.filter((n) => n.is_orphan) : []),
    [analysis],
  );

  async function runAutoSort() {
    if (!analysis || autoSortRunning) return;
    setAutoSortRunning(true);
    setAutoSortResult(null);
    try {
      const result = await invoke<string>("vault_auto_sort", {});
      setAutoSortResult(result);
      void load();
    } catch (e) {
      setAutoSortResult(`Error: ${humanizeError(e)}`);
    } finally {
      setAutoSortRunning(false);
    }
  }

  const NAV: { id: SubView; label: string }[] = [
    { id: "overview", label: "Overview" },
    { id: "folders", label: "Folders" },
    { id: "tags", label: "Tags" },
    { id: "notes", label: "All Notes" },
    { id: "orphans", label: "Orphans" },
    { id: "broken", label: "Broken Links" },
  ];

  return (
    <div className="vault-manager">
      <div className="vault-toolbar">
        <div className="vault-nav">
          {NAV.map((n) => (
            <button
              key={n.id}
              type="button"
              className={`vault-nav-btn${view === n.id ? " active" : ""}`}
              onClick={() => setView(n.id)}
            >
              {n.label}
              {n.id === "orphans" && analysis ? ` (${orphans.length})` : ""}
              {n.id === "broken" && analysis ? ` (${analysis.broken_link_count})` : ""}
            </button>
          ))}
        </div>
        <div className="vault-actions">
          <button
            type="button"
            className="link-btn"
            onClick={() => void load()}
            disabled={loading}
          >
            {loading ? "..." : "Refresh"}
          </button>
          <button
            type="button"
            className="link-btn"
            onClick={() => setTab("knowledge-graph")}
            title="View wikilink topology"
          >
            Graph
          </button>
        </div>
      </div>

      {error ? (
        <div className="vault-error">{error}</div>
      ) : loading && !analysis ? (
        <div className="muted" style={{ padding: 16 }}>Scanning vault...</div>
      ) : !analysis ? null : (
        <div className="vault-body">
          {view === "overview" && (
            <>
              <div className="vault-stats-row">
                <StatCard label="Notes" value={analysis.total_notes} onClick={() => setView("notes")} />
                <StatCard label="Folders" value={analysis.total_folders} onClick={() => setView("folders")} />
                <StatCard label="Tags" value={analysis.total_tags} onClick={() => setView("tags")} />
                <StatCard
                  label="Orphans"
                  value={analysis.orphan_count}
                  onClick={() => setView("orphans")}
                  warn={analysis.orphan_count > 0}
                />
                <StatCard
                  label="Broken Links"
                  value={analysis.broken_link_count}
                  onClick={() => setView("broken")}
                  warn={analysis.broken_link_count > 0}
                />
              </div>

              {analysis.tags.length > 0 && (
                <div className="vault-section">
                  <div className="vault-section-header">
                    <span>Top Tags</span>
                    <button type="button" className="link-btn" onClick={() => setView("tags")}>all</button>
                  </div>
                  <div className="vault-tag-cloud">
                    {analysis.tags.slice(0, 20).map((t) => (
                      <span key={t.tag} className="vault-tag-pill" title={`${t.count} notes`}>
                        #{t.tag}
                        <span className="vault-tag-count">{t.count}</span>
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {analysis.folders.length > 1 && (
                <div className="vault-section">
                  <div className="vault-section-header">
                    <span>Top Folders</span>
                    <button type="button" className="link-btn" onClick={() => setView("folders")}>all</button>
                  </div>
                  <div className="vault-top-folders">
                    {analysis.folders
                      .filter((f) => f.path !== "/" && !f.path.includes("/"))
                      .slice(0, 10)
                      .map((f) => (
                        <div key={f.path} className="vault-folder-row">
                          <span className="vault-folder-name">{f.path}</span>
                          <span className="muted">{f.total_count} notes</span>
                        </div>
                      ))}
                  </div>
                </div>
              )}

              <div className="vault-section">
                <div className="vault-section-header">
                  <span>AI Auto-Sort</span>
                </div>
                <p className="muted" style={{ margin: "0 0 8px" }}>
                  Analyze your vault's topology and get AI-powered suggestions for folder moves,
                  tag cleanup, orphan resolution, and link tightening.
                </p>
                <button
                  type="button"
                  className="vault-autosort-btn"
                  onClick={() => void runAutoSort()}
                  disabled={autoSortRunning}
                >
                  {autoSortRunning ? "Analyzing..." : "Run Auto-Sort Analysis"}
                </button>
                {autoSortResult && (
                  <pre className="vault-autosort-result">{autoSortResult}</pre>
                )}
              </div>
            </>
          )}

          {view === "folders" && <FolderList analysis={analysis} />}
          {view === "tags" && <TagCloud analysis={analysis} />}
          {view === "notes" && (
            <NoteList notes={analysis.notes} label="All notes" onOpen={openNote} />
          )}
          {view === "orphans" && (
            <NoteList notes={orphans} label="Orphan notes" onOpen={openNote} />
          )}
          {view === "broken" && (
            <BrokenLinkList analysis={analysis} onOpen={openNote} />
          )}
        </div>
      )}
    </div>
  );
}
