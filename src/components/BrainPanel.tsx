import { useEffect, useState } from "react";
import { brainSnapshot, type BrainSnapshot } from "@/lib/brain";
import { timeAgo } from "@/lib/time";
import { openProjectByPath } from "@/lib/open-project";
import { pushToast } from "@/lib/toast";
import { PanelLoading } from "./Skeleton";
import { useCortexStore } from "@/state/store";

export function BrainPanel() {
  const [snap, setSnap] = useState<BrainSnapshot | null>(null);
  const [tab, setTab] = useState<"sessions" | "projects" | "memory">("sessions");

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const s = await brainSnapshot();
        if (mounted) setSnap(s);
      } catch { /* backend warming */ }
    };
    void tick();
    const id = setInterval(tick, 8_000);
    return () => { mounted = false; clearInterval(id); };
  }, []);

  if (!snap) return <PanelLoading label="Loading brain" />;

  return (
    <div className="brain-panel">
      <div className="brain-tabs">
        <button className={tab === "sessions" ? "active" : ""} onClick={() => setTab("sessions")}>
          sessions <span className="badge">{snap.recent_sessions.length}</span>
        </button>
        <button className={tab === "projects" ? "active" : ""} onClick={() => setTab("projects")}>
          projects <span className="badge">{snap.recent_projects.length}</span>
        </button>
        <button className={tab === "memory" ? "active" : ""} onClick={() => setTab("memory")}>
          memory <span className="badge">{snap.recent_memory.length}</span>
        </button>
      </div>
      <div className="brain-body">
        {tab === "sessions" && (
          <div className="brain-list">
            {snap.recent_sessions.length === 0 && (
              <div className="muted">No sessions yet. Start chatting and they'll show up here.</div>
            )}
            {snap.recent_sessions.map((s) => {
              const resume = () => {
                window.dispatchEvent(
                  new CustomEvent("cortex:chat-replay", {
                    detail: { session_id: s.session_id },
                  }),
                );
              };
              return (
                <div
                  key={s.session_id}
                  className="brain-row brain-row-interactive"
                  role="button"
                  tabIndex={0}
                  onClick={resume}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      resume();
                    }
                  }}
                  title="Click to resume this session in chat"
                >
                  <div className="brain-row-head">
                    <strong>{s.first_message ?? `session ${s.session_id.slice(-8)}`}</strong>
                    <span className="muted">{timeAgo(s.last_active_ms)}</span>
                  </div>
                  <div className="muted brain-meta">
                    {s.message_count} msgs · {s.agents.filter(Boolean).join(", ") || "—"}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        {tab === "projects" && (
          <div className="brain-list">
            {snap.recent_projects.length === 0 && <div className="muted">No projects in ~/projects.</div>}
            {snap.recent_projects.map((p) => {
              // Same hand-off the Projects sidebar rows run (backend
              // set_active_project + store + chat bootstrap), then reveal the
              // Projects tab — matching the interaction affordance of the
              // session/memory sibling rows above and below.
              const openProject = () => {
                void openProjectByPath(p.root).then((found) => {
                  if (!found) {
                    pushToast({
                      title: "Project not registered",
                      body: `${p.root} isn't in the project registry — open it from the Projects sidebar roots.`,
                      kind: "info",
                    });
                  }
                });
              };
              return (
                <div
                  key={p.root}
                  className="brain-row brain-row-interactive"
                  role="button"
                  tabIndex={0}
                  onClick={openProject}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      openProject();
                    }
                  }}
                  title="Click to make this the active project"
                >
                  <div className="brain-row-head">
                    <strong>{p.name}</strong>
                    <span className="muted">{timeAgo(p.last_modified_ms)}</span>
                  </div>
                  <div className="muted brain-meta">
                    {[p.has_git && "git", p.has_claude_md && "claude", p.has_runbooks && "runbooks"]
                      .filter(Boolean)
                      .join(" · ") || "—"}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        {tab === "memory" && (
          <div className="brain-list">
            {snap.obsidian_vault === null ? (
              <div className="brain-banner">
                No Obsidian vault detected. Drop notes in <code>~/Documents/Cortex Brain</code> or
                point Settings → Workspace at your vault.
              </div>
            ) : (
              <div className="brain-banner" style={{ borderLeftColor: "var(--success)" }}>
                ✓ Vault: <code>{snap.obsidian_vault}</code>
              </div>
            )}
            {snap.recent_memory.length === 0 && (
              <div className="muted">No memory files indexed yet — try the Memory tab (Ctrl+Shift+F) for full search.</div>
            )}
            {snap.recent_memory.map((m) => {
              const openEditor = () => {
                useCortexStore.getState().setActivityTab("editor");
                setTimeout(() => {
                  window.dispatchEvent(
                    new CustomEvent("cortex:editor-open", { detail: { path: m.path } }),
                  );
                }, 0);
              };
              return (
                <div
                  key={m.path}
                  className="brain-row brain-row-interactive"
                  role="button"
                  tabIndex={0}
                  onClick={openEditor}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      openEditor();
                    }
                  }}
                  title="Click to open in the editor"
                >
                  <div className="brain-row-head">
                    <strong>{m.title ?? basename(m.path)}</strong>
                    <span className="muted">{timeAgo(m.modified_unix_ms)}</span>
                  </div>
                  <div className="muted brain-meta">{m.source}</div>
                  <div className="brain-preview">{m.preview}</div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function basename(p: string): string {
  const m = p.match(/([^/\\]+)$/);
  return m ? m[1] : p;
}
