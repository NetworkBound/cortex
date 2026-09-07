import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { BookText, GitBranch } from "lucide-react";
import { openCodeProject } from "@/lib/open-project";
import { listProjects, openVaultNote, type ProjectMeta } from "@/lib/projects";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { FileExplorer } from "./FileExplorer";
import { WorktreePicker } from "./WorktreePicker";
import "../styles/project-sidebar.css";

/**
 * Window event the `/worktree` slash command dispatches to pop the
 * worktree picker open from anywhere (the picker is prop-driven, so this
 * bridges the chat command to the sidebar's local open state).
 */
export const OPEN_WORKTREES_EVENT = "cortex:open-worktrees";

export function ProjectSidebar() {
  const projects = useCortexStore((s) => s.projects);
  const active = useCortexStore((s) => s.activeProject);
  const setProjects = useCortexStore((s) => s.setProjects);
  const setActive = useCortexStore((s) => s.setActiveProject);
  const [worktreesOpen, setWorktreesOpen] = useState(false);

  useEffect(() => {
    listProjects().then(setProjects).catch(() => {});
    // Re-fetch when the backend registers a new project (Setup's
    // "Clone & connect") so the new repo appears without a remount.
    let unlisten: (() => void) | undefined;
    let disposed = false;
    listen("projects:changed", () => {
      listProjects().then(setProjects).catch(() => {});
    })
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [setProjects]);

  // Let `/worktree` (and any other caller) open the picker via a window event.
  useEffect(() => {
    const onOpen = () => setWorktreesOpen(true);
    window.addEventListener(OPEN_WORKTREES_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_WORKTREES_EVENT, onOpen);
  }, []);

  // Group projects by their `group` header. Code groups come first in the
  // order the backend surfaced them; "Vault Projects" is always pinned last.
  const groups = useMemo(() => {
    const order: string[] = [];
    const byGroup = new Map<string, ProjectMeta[]>();
    for (const p of projects) {
      if (!byGroup.has(p.group)) {
        byGroup.set(p.group, []);
        order.push(p.group);
      }
      byGroup.get(p.group)!.push(p);
    }
    order.sort((a, b) => {
      const av = a === "Vault Projects" ? 1 : 0;
      const bv = b === "Vault Projects" ? 1 : 0;
      if (av !== bv) return av - bv;
      return order.indexOf(a) - order.indexOf(b);
    });
    return order.map((g) => ({ group: g, items: byGroup.get(g)! }));
  }, [projects]);

  async function pickVault(p: ProjectMeta) {
    // Vault notes are FILES, not dirs — `set_active_project` would reject
    // them. Just highlight the row and inject the note as chat context.
    setActive(p);
    try {
      if (p.note_path) {
        const content = await openVaultNote(p.note_path);
        useCortexStore.getState().appendMessage({
          id: `vault-${crypto.randomUUID()}`,
          role: "system",
          content: `📓 Loaded vault project **${p.name}**\n\n${content}`,
          tools: [],
        });
        pushToast({
          title: `Loaded: ${p.name}`,
          body: "Vault note injected into chat as context.",
          kind: "success",
        });
      } else {
        pushToast({
          title: `Active: ${p.name}`,
          body: "Vault project selected (no backing note to load).",
          kind: "info",
        });
      }
    } catch (err) {
      console.warn("openVaultNote failed", err);
      pushToast({
        title: p.name,
        body: "Could not load vault note.",
        kind: "error",
      });
    }
  }

  async function pick(p: ProjectMeta) {
    if (p.kind === "vault") {
      await pickVault(p);
      return;
    }
    // Shared with Setup's "Open project" hand-off: backend switch, store
    // update, and chat session bootstrap all live in lib/open-project.
    await openCodeProject(p);
  }

  return (
    <aside className="sidebar project-sidebar">
      <div className="sidebar-section-head">
        <h2>Projects</h2>
        <span className="sidebar-count">{projects.length}</span>
      </div>
      {projects.length === 0 && (
        <div className="sidebar-empty">
          <div className="sidebar-empty-icon">∅</div>
          <div className="sidebar-empty-title">No projects yet</div>
          <div className="sidebar-empty-sub">
            Drop a folder into <code>~/projects/</code> (or set <code>CORTEX_PROJECTS_ROOT</code>),
            or add a project note under <code>30-Projects/</code> in your vault — Cortex surfaces both here.
          </div>
        </div>
      )}
      <div className="project-list">
        {groups.map(({ group, items }) => (
          <div key={group} className="project-group">
            <div className="project-group-head">
              <span className="project-group-name">{group}</span>
              <span className="project-group-count">{items.length}</span>
            </div>
            {items.map((p) => (
              <button
                key={p.root}
                className={`project-row ${active?.root === p.root ? "active" : ""}`}
                onClick={() => void pick(p)}
              >
                <div>
                  <strong>{p.name}</strong>
                  <div className="project-row-meta">
                    {p.kind === "vault" ? (
                      <>
                        <span className="meta-chip meta-chip-vault">
                          <BookText size={12} strokeWidth={1.75} aria-hidden="true" /> vault
                        </span>
                        {p.subtitle && <span className="project-row-subtitle">{p.subtitle}</span>}
                      </>
                    ) : (
                      <>
                        {p.has_git && <span className="meta-chip">git</span>}
                        {p.has_claude_md && <span className="meta-chip">claude</span>}
                        {p.has_runbooks && <span className="meta-chip">runbooks</span>}
                        {!p.has_git && !p.has_claude_md && !p.has_runbooks && (
                          <span className="muted">—</span>
                        )}
                      </>
                    )}
                  </div>
                </div>
              </button>
            ))}
          </div>
        ))}
      </div>

      {active?.kind === "vault" ? (
        <div className="sidebar-section-head" style={{ marginTop: 16 }}>
          <h2>Vault note</h2>
          <span className="sidebar-vault-hint">loaded into chat</span>
        </div>
      ) : (
        <>
          <div className="sidebar-section-head" style={{ marginTop: 16 }}>
            <h2>{active ? `${active.name}/` : "Files"}</h2>
            <button
              className="sidebar-action-btn"
              onClick={() => setWorktreesOpen(true)}
              disabled={!active}
              title={active ? "Manage git worktrees" : "Pick an active project first"}
              aria-label="Worktrees"
            >
              <GitBranch size={13} strokeWidth={1.75} aria-hidden="true" /> Worktrees
            </button>
          </div>
          <FileExplorer root={active?.root ?? null} projectName={active?.name} />
        </>
      )}

      <WorktreePicker open={worktreesOpen} onClose={() => setWorktreesOpen(false)} />
    </aside>
  );
}
