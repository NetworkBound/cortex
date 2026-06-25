import { useEffect, useState } from "react";
import { getProjects } from "../lib/api";
import { useStore } from "../lib/store";
import { projectName, projectPath, type Project } from "../lib/types";

export default function ProjectsView() {
  const { activeProject, setActiveProject } = useStore();
  const [projects, setProjects] = useState<Project[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = () => {
    setLoading(true);
    getProjects()
      .then((p) => {
        setProjects(Array.isArray(p) ? p : []);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(refresh, []);

  // Group by `group` field, defensively.
  const groups = new Map<string, Project[]>();
  for (const p of projects) {
    const g = (p.group as string) || "Projects";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g)!.push(p);
  }

  const isActive = (p: Project) =>
    activeProject && projectPath(activeProject) === projectPath(p);

  return (
    <div className="scroll">
      {error && <div className="banner err">{error}</div>}
      {loading && projects.length === 0 && <div className="empty">Loading projects…</div>}
      {!loading && projects.length === 0 && !error && (
        <div className="empty">No projects found.</div>
      )}

      {activeProject && (
        <div className="banner">
          Active: <strong>{projectName(activeProject)}</strong>
          <button
            className="btn"
            style={{ marginLeft: 10, padding: "4px 10px", fontSize: 12 }}
            onClick={() => setActiveProject(null)}
          >
            Clear
          </button>
        </div>
      )}

      <div className="list">
        {[...groups.entries()].map(([group, items]) => (
          <div key={group}>
            <div className="group-head">{group}</div>
            {items.map((p, i) => (
              <button
                key={projectPath(p) || `${group}-${i}`}
                className={`row-item ${isActive(p) ? "selected" : ""}`}
                onClick={() => setActiveProject(p)}
              >
                <div className="meta">
                  <div className="name">{projectName(p)}</div>
                  <div className="sub">
                    {(p.subtitle as string) || projectPath(p) || (p.kind as string) || ""}
                  </div>
                </div>
                {isActive(p) && <span className="check">✓</span>}
              </button>
            ))}
          </div>
        ))}
      </div>

      <div className="pad">
        <button className="btn" style={{ width: "100%" }} onClick={refresh}>
          Refresh
        </button>
      </div>
    </div>
  );
}
