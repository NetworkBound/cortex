import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { useCortexStore } from "@/state/store";
import {
  gitCommitFileDiff,
  gitCommitFiles,
  gitHistory,
  type Commit,
  type CommitFile,
} from "@/lib/git";
import { parseUnifiedDiff } from "@/lib/diff";
import { assignLanes, laneColor, type LanedCommit } from "@/lib/git-graph";

/**
 * Git-log viewer with DAG lane rendering.
 *
 * - Loads the first `PAGE_SIZE` commits on mount + refreshes the first page
 *   every 15s. A "Load more" button pages deeper via an offset cursor.
 * - Filters client-side by subject/hash/author.
 * - Renders a SourceTree-style mini-graph (SVG, colored lanes per branch).
 * - Click a row to expand → lists the files that commit touched; clicking a
 *   file fetches and renders that single file's diff (reusing the shared
 *   unified-diff parser + the `hunk-*` row styling).
 */

const LANE_WIDTH = 14;
const ROW_HEIGHT = 26;
const NODE_RADIUS = 4;
const STROKE_WIDTH = 1.5;
const LEFT_PAD = 8;

/** Commits fetched per page. Refresh always re-reads page 0 at this width. */
const PAGE_SIZE = 100;

export function GitHistoryPanel() {
  const project = useCortexStore((s) => s.activeProject);
  const [commits, setCommits] = useState<Commit[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<string | null>(null);
  // Pagination: how many commits we've asked for so far, and whether the last
  // page came back full (a short page means we hit the bottom of history).
  const [loadingMore, setLoadingMore] = useState(false);
  const [atEnd, setAtEnd] = useState(false);

  const root = project?.root ?? null;

  // Reset paging + expansion whenever the project changes.
  useEffect(() => {
    setCommits([]);
    setExpanded(null);
    setAtEnd(false);
    setError(null);
  }, [root]);

  useEffect(() => {
    if (!root) return;
    let cancelled = false;
    // Refresh re-reads only the first page so polling stays cheap; any deeper
    // pages the user loaded are preserved by merging on hash below.
    const load = async () => {
      setLoading(true);
      setError(null);
      try {
        const rows = await gitHistory(root, PAGE_SIZE, 0);
        if (cancelled) return;
        setCommits((prev) => {
          if (prev.length <= PAGE_SIZE) return rows;
          // Keep the already-loaded tail (offset >= PAGE_SIZE) intact.
          const tail = prev.slice(PAGE_SIZE);
          const seen = new Set(rows.map((c) => c.hash));
          return [...rows, ...tail.filter((c) => !seen.has(c.hash))];
        });
        if (rows.length < PAGE_SIZE) setAtEnd(true);
      } catch (e) {
        if (!cancelled) setError(humanizeError(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    const id = setInterval(load, 15_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [root]);

  const loadMore = useCallback(async () => {
    if (!root || loadingMore || atEnd) return;
    setLoadingMore(true);
    setError(null);
    try {
      const rows = await gitHistory(root, PAGE_SIZE, commits.length);
      const seen = new Set(commits.map((c) => c.hash));
      const next = rows.filter((c) => !seen.has(c.hash));
      setCommits((prev) => [...prev, ...next]);
      if (rows.length < PAGE_SIZE || next.length === 0) setAtEnd(true);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoadingMore(false);
    }
  }, [root, commits, loadingMore, atEnd]);

  // Lane assignment runs against the *unfiltered* list — filtering rows out
  // would break the parent/child threading. We filter only for display below.
  const laned = useMemo(() => assignLanes(commits), [commits]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const pairs = commits.map((c, i) => ({ commit: c, lane: laned[i] }));
    if (!q) return pairs;
    return pairs.filter(
      ({ commit: c }) =>
        c.subject.toLowerCase().includes(q) ||
        c.short_hash.toLowerCase().includes(q) ||
        c.author.toLowerCase().includes(q),
    );
  }, [commits, laned, query]);

  // Width is the largest lane count across the *whole* (unfiltered) graph, so
  // every row's SVG aligns to the same grid even when search trims the list.
  const graphWidth = useMemo(() => {
    let max = 1;
    for (const l of laned) if (l.width > max) max = l.width;
    return max * LANE_WIDTH + LEFT_PAD * 2;
  }, [laned]);

  function toggleExpand(hash: string) {
    setExpanded((cur) => (cur === hash ? null : hash));
  }

  if (!root) {
    return (
      <div className="git-history-empty muted">
        Open a project to see its git history.
      </div>
    );
  }

  const showInitialLoading = loading && commits.length === 0;
  const showEmpty = !loading && commits.length === 0 && !error;

  return (
    <div className="git-history">
      <div className="git-history-head">
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter commits…"
          className="git-history-search"
        />
        <span className="muted git-history-count">
          {filtered.length} / {commits.length}
        </span>
      </div>
      {error && <div className="git-history-error">{error}</div>}
      {showInitialLoading && (
        <div className="muted git-history-loading">Reading commit history…</div>
      )}
      {showEmpty && (
        <div className="git-history-empty muted">
          No commits here yet. This branch has no history, or the folder isn't a
          git repository.
        </div>
      )}
      <div className="git-history-list git-history-list-graph">
        {filtered.map(({ commit: c, lane }) => (
          <div key={c.hash} className="git-commit">
            <button
              type="button"
              className="git-commit-row git-commit-row-graph"
              onClick={() => toggleExpand(c.hash)}
              aria-expanded={expanded === c.hash}
            >
              <GraphCell lane={lane} width={graphWidth} />
              <span className="git-commit-info">
                <span className="git-commit-line1">
                  <span className="git-commit-hash">{c.short_hash}</span>
                  <span className="git-commit-subject">{c.subject}</span>
                  <span className="git-commit-meta">
                    {c.author} · {c.age}
                  </span>
                </span>
                {c.refs.length > 0 && (
                  <span className="git-commit-refs">
                    {c.refs.map((r, i) => (
                      <span key={i} className="git-commit-ref">
                        {r}
                      </span>
                    ))}
                  </span>
                )}
              </span>
            </button>
            {expanded === c.hash && (
              <CommitDetail root={root} hash={c.hash} />
            )}
          </div>
        ))}
        {!query && commits.length > 0 && (
          <div className="git-history-more">
            {atEnd ? (
              <span className="muted git-history-more-end">
                End of history
              </span>
            ) : (
              <button
                type="button"
                className="git-history-more-btn"
                onClick={() => void loadMore()}
                disabled={loadingMore}
              >
                {loadingMore ? "Loading…" : "Load more"}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Expanded commit body: the list of files the commit touched. Selecting a file
 * fetches and renders that file's diff inline. Loads lazily on mount (i.e. when
 * the parent row is expanded).
 */
function CommitDetail({ root, hash }: { root: string; hash: string }) {
  const [files, setFiles] = useState<CommitFile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setFiles(null);
    setError(null);
    setSelected(null);
    (async () => {
      try {
        const f = await gitCommitFiles(root, hash);
        if (!cancelled) setFiles(f);
      } catch (e) {
        if (!cancelled) setError(humanizeError(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [root, hash]);

  if (error) {
    return <div className="git-commit-detail-error">{error}</div>;
  }
  if (files === null) {
    return (
      <div className="git-commit-detail muted git-commit-detail-loading">
        Loading changed files…
      </div>
    );
  }
  if (files.length === 0) {
    return (
      <div className="git-commit-detail muted git-commit-detail-empty">
        No file changes in this commit (it may be a merge or an empty commit).
      </div>
    );
  }

  return (
    <div className="git-commit-detail">
      <div className="git-commit-files">
        {files.map((f) => (
          <button
            key={f.path}
            type="button"
            className={`git-commit-file${
              selected === f.path ? " is-active" : ""
            }`}
            onClick={() =>
              setSelected((cur) => (cur === f.path ? null : f.path))
            }
            aria-expanded={selected === f.path}
            title={f.path}
          >
            <span className={`git-commit-file-status status-${f.status}`}>
              {f.status}
            </span>
            <span className="git-commit-file-path">{f.path}</span>
          </button>
        ))}
      </div>
      {selected && (
        <FileDiff root={root} hash={hash} path={selected} />
      )}
    </div>
  );
}

/** Fetch + render the unified diff for a single file inside a commit. */
function FileDiff({
  root,
  hash,
  path,
}: {
  root: string;
  hash: string;
  path: string;
}) {
  const [diff, setDiff] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setDiff(null);
    setError(null);
    (async () => {
      try {
        const text = await gitCommitFileDiff(root, hash, path);
        if (!cancelled) setDiff(text);
      } catch (e) {
        if (!cancelled) setError(humanizeError(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [root, hash, path]);

  if (error) {
    return <div className="git-commit-detail-error">{error}</div>;
  }
  if (diff === null) {
    return (
      <div className="git-file-diff-loading muted">Loading diff…</div>
    );
  }

  const parsed = parseUnifiedDiff(diff);
  if (parsed.totalRows === 0) {
    return (
      <div className="git-file-diff-empty muted">
        No textual diff to show — this file is binary, or only its mode changed.
      </div>
    );
  }

  return (
    <pre className="git-file-diff hunk-body">
      {parsed.hunks.map((hunk, hi) => (
        <div key={hi} className="git-file-diff-hunk">
          <div className="git-file-diff-hunk-header">
            @@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},
            {hunk.newCount} @@
          </div>
          {hunk.rows.map((row, ri) => (
            <div key={ri} className={`hunk-row hunk-row-${row.kind}`}>
              <span className="hunk-marker">
                {row.kind === "add" ? "+" : row.kind === "del" ? "-" : " "}
              </span>
              <span className="hunk-text">{row.text}</span>
            </div>
          ))}
        </div>
      ))}
    </pre>
  );
}

/**
 * Draws one row of the graph: a colored node on this commit's lane plus lines
 * descending to each parent's lane. The next row's node sits on `parentLanes[0]`
 * so consecutive cells stitch together cleanly into a continuous DAG.
 */
function GraphCell({ lane, width }: { lane: LanedCommit; width: number }) {
  const cx = LEFT_PAD + lane.lane * LANE_WIDTH;
  const cy = ROW_HEIGHT / 2;

  // Down-stroke to each parent lane. Lines go from the node center to the
  // bottom of *this* row (the next row picks up from y=0).
  const lines = lane.parentLanes.map((parentLane, idx) => {
    const x2 = LEFT_PAD + parentLane * LANE_WIDTH;
    const color = laneColor(parentLane);
    return (
      <line
        key={idx}
        x1={cx}
        y1={cy}
        x2={x2}
        y2={ROW_HEIGHT}
        stroke={color}
        strokeWidth={STROKE_WIDTH}
        strokeLinecap="round"
      />
    );
  });

  return (
    <svg
      className="git-graph-cell"
      width={width}
      height={ROW_HEIGHT}
      viewBox={`0 0 ${width} ${ROW_HEIGHT}`}
      aria-hidden="true"
    >
      {lines}
      <circle
        cx={cx}
        cy={cy}
        r={NODE_RADIUS}
        fill={lane.color}
        stroke="var(--bg, #111)"
        strokeWidth={STROKE_WIDTH}
      />
    </svg>
  );
}
