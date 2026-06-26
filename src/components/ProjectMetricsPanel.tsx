import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  formatBytes,
  formatCount,
  projectMetrics,
  type ProjectMetrics,
} from "@/lib/project-metrics";
import { useCortexStore } from "@/state/store";

/**
 * Activity-panel tab that renders the output of the `project_metrics` backend
 * command for the currently active project. Four stat cards at the top
 * (files / lines / languages / size), a CSS-only language bar chart in the
 * middle, then tables of the largest files + biggest first-level dirs.
 *
 * The backend is read-only and recomputes on every refresh — no caching here.
 */
export function ProjectMetricsPanel() {
  const project = useCortexStore((s) => s.activeProject);
  const [metrics, setMetrics] = useState<ProjectMetrics | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!project) {
      setMetrics(null);
      setError(null);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const m = await projectMetrics(String(project.root));
      setMetrics(m);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, [project]);

  // Kick off the initial walk whenever the active project changes.
  useEffect(() => {
    void reload();
  }, [reload]);

  if (!project) {
    return (
      <div className="metrics-empty">
        Pick a project from the sidebar to see code metrics.
      </div>
    );
  }

  return (
    <div className="metrics-panel">
      <div className="metrics-head">
        <div>
          <h3 className="metrics-title">{project.name}</h3>
          <div className="metrics-sub" title={String(project.root)}>
            {String(project.root)}
          </div>
        </div>
        <button
          className="metrics-refresh"
          onClick={() => void reload()}
          disabled={loading}
        >
          {loading ? "Scanning…" : "Refresh"}
        </button>
      </div>

      {error && (
        <div className="metrics-error">
          <strong>Scan failed.</strong>
          <pre>{error}</pre>
        </div>
      )}

      {!metrics && loading && (
        <div className="metrics-loading">Scanning project tree…</div>
      )}

      {metrics && (
        <>
          <MetricsCards m={metrics} />
          <LanguageChart m={metrics} />
          <LargestFiles m={metrics} />
          <BiggestDirs m={metrics} />
          {metrics.truncated && (
            <div className="metrics-warn">
              ⚠ Scan capped at 50,000 entries — numbers may underrepresent
              the full tree.
            </div>
          )}
        </>
      )}
    </div>
  );
}

function MetricsCards({ m }: { m: ProjectMetrics }) {
  const langCount = Object.keys(m.languages).length;
  return (
    <div className="metrics-cards">
      <StatCard label="Files" value={formatCount(m.total_files)} />
      <StatCard label="Lines" value={formatCount(m.total_lines)} />
      <StatCard label="Languages" value={formatCount(langCount)} />
      <StatCard label="Size" value={formatBytes(m.total_bytes)} />
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="metrics-card">
      <div className="metrics-card-value">{value}</div>
      <div className="metrics-card-label">{label}</div>
    </div>
  );
}

function LanguageChart({ m }: { m: ProjectMetrics }) {
  // Sort by line count desc, drop zero-line buckets (e.g. binary "Other"
  // entries with no countable lines).
  const rows = useMemo(() => {
    const all = Object.entries(m.languages).map(([name, stat]) => ({
      name,
      ...stat,
    }));
    return all
      .filter((r) => r.lines > 0)
      .sort((a, b) => b.lines - a.lines)
      .slice(0, 12);
  }, [m]);
  if (rows.length === 0) {
    return (
      <section className="metrics-section">
        <h4>Language breakdown</h4>
        <div className="metrics-empty-row">No countable source lines yet.</div>
      </section>
    );
  }
  const max = rows[0].lines;
  return (
    <section className="metrics-section">
      <h4>Language breakdown</h4>
      <div className="metrics-bars">
        {rows.map((r) => (
          <div className="metrics-bar-row" key={r.name}>
            <div className="metrics-bar-name" title={r.name}>
              {r.name}
            </div>
            <div className="metrics-bar-track">
              <div
                className="metrics-bar-fill"
                style={{ width: `${Math.max(2, (r.lines / max) * 100)}%` }}
              />
            </div>
            <div className="metrics-bar-stats">
              <span>{formatCount(r.lines)} lines</span>
              <span aria-hidden>·</span>
              <span>{formatCount(r.files)} files</span>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

function LargestFiles({ m }: { m: ProjectMetrics }) {
  if (m.largest_files.length === 0) return null;
  return (
    <section className="metrics-section">
      <h4>Largest files</h4>
      <table className="metrics-table">
        <thead>
          <tr>
            <th>Path</th>
            <th className="num">Lines</th>
            <th className="num">Size</th>
          </tr>
        </thead>
        <tbody>
          {m.largest_files.map((f) => (
            <tr key={f.path}>
              <td className="path" title={f.path}>
                {shortenPath(f.path, m.project_root)}
              </td>
              <td className="num">{formatCount(f.lines)}</td>
              <td className="num">{formatBytes(f.bytes)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function BiggestDirs({ m }: { m: ProjectMetrics }) {
  if (m.biggest_dirs.length === 0) return null;
  return (
    <section className="metrics-section">
      <h4>Biggest directories</h4>
      <table className="metrics-table">
        <thead>
          <tr>
            <th>Path</th>
            <th className="num">Files</th>
            <th className="num">Size</th>
          </tr>
        </thead>
        <tbody>
          {m.biggest_dirs.map((d) => (
            <tr key={d.path}>
              <td className="path" title={d.path}>
                {shortenPath(d.path, m.project_root)}
              </td>
              <td className="num">{formatCount(d.file_count)}</td>
              <td className="num">{formatBytes(d.total_bytes)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

/** Trim the project root prefix so the table cells stay readable. */
function shortenPath(path: string, root: string): string {
  if (!root) return path;
  const norm = path.replace(/\\/g, "/");
  const r = root.replace(/\\/g, "/").replace(/\/$/, "");
  if (norm.startsWith(r + "/")) return norm.slice(r.length + 1);
  if (norm === r) return ".";
  return norm;
}
