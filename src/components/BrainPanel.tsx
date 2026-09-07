import { useEffect, useState } from "react";
import {
  brainSnapshot,
  brainRag,
  brainMemoryDuplicates,
  type BrainSnapshot,
  type BrainAnswer,
  type DuplicateGroup,
} from "@/lib/brain";
import { timeAgo } from "@/lib/time";
import { openProjectByPath } from "@/lib/open-project";
import { pushToast } from "@/lib/toast";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";
import { useCortexStore } from "@/state/store";

export function BrainPanel() {
  const [snap, setSnap] = useState<BrainSnapshot | null>(null);
  const [tab, setTab] = useState<"ask" | "sessions" | "projects" | "memory">("ask");
  // Memory-tab dedup scan state.
  const [dupGroups, setDupGroups] = useState<DuplicateGroup[] | null>(null);
  const [dupLoading, setDupLoading] = useState(false);
  const [dupError, setDupError] = useState<string | null>(null);
  const runDedupScan = async () => {
    if (dupLoading) return;
    setDupLoading(true);
    setDupError(null);
    try {
      setDupGroups(await brainMemoryDuplicates());
    } catch (e) {
      setDupError(humanizeError(e));
    } finally {
      setDupLoading(false);
    }
  };
  // "Ask your brain" (unified RAG) state.
  const [question, setQuestion] = useState("");
  const [asking, setAsking] = useState(false);
  const [answer, setAnswer] = useState<BrainAnswer | null>(null);
  const [askError, setAskError] = useState<string | null>(null);
  const runAsk = async () => {
    const q = question.trim();
    if (!q || asking) return;
    setAsking(true);
    setAskError(null);
    setAnswer(null);
    try {
      // Scope retrieval to the active project (+ global fallback) so one
      // project's runbooks/instructions never leak into another's answers.
      const projectRoot = useCortexStore.getState().activeProject?.root ?? null;
      setAnswer(await brainRag(q, 8, projectRoot));
    } catch (e) {
      setAskError(humanizeError(e));
    } finally {
      setAsking(false);
    }
  };

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
        <button className={tab === "ask" ? "active" : ""} onClick={() => setTab("ask")}>
          ask
        </button>
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
        {tab === "ask" && (
          <div className="brain-ask">
            <div className="brain-ask-bar">
              <input
                value={question}
                onChange={(e) => setQuestion(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void runAsk();
                  }
                }}
                placeholder="Ask your brain — grounded in your notes + past chats…"
                disabled={asking}
              />
              <button onClick={() => void runAsk()} disabled={asking || !question.trim()}>
                {asking ? "Thinking…" : "Ask"}
              </button>
            </div>
            {askError && <div className="settings-err">{askError}</div>}
            {asking && <div className="muted">Searching your notes + chats…</div>}
            {!answer && !asking && !askError && (
              <div className="muted brain-ask-hint">
                Ask anything about what you've discussed or noted. Cortex retrieves from your
                imported chats + Obsidian vault and answers with citations — all local.
              </div>
            )}
            {answer && (
              <div className="brain-answer">
                {!answer.used_context && (
                  <div className="muted">No relevant context found in your brain.</div>
                )}
                <div className="brain-answer-body">{answer.answer}</div>
                {answer.citations.length > 0 && (
                  <div className="brain-cites">
                    <div className="muted brain-cites-head">
                      Sources · answered by {answer.model}
                    </div>
                    {answer.citations.map((c) => {
                      const isChat = c.source === "chat";
                      const open = () => {
                        if (isChat) {
                          window.dispatchEvent(
                            new CustomEvent("cortex:chat-replay", {
                              detail: { session_id: c.reference },
                            }),
                          );
                        } else {
                          // Notes can live outside the vault (e.g. ~/.claude
                          // memories, runbooks), so the backend supplies an
                          // absolute open_path; fall back to vault + reference.
                          const path = c.open_path
                            ? c.open_path
                            : snap.obsidian_vault
                              ? `${snap.obsidian_vault}\\${c.reference}`
                              : c.reference;
                          useCortexStore.getState().setActivityTab("editor");
                          setTimeout(() => {
                            window.dispatchEvent(
                              new CustomEvent("cortex:editor-open", { detail: { path } }),
                            );
                          }, 0);
                        }
                      };
                      return (
                        <div
                          key={c.n}
                          className="brain-cite brain-row-interactive"
                          role="button"
                          tabIndex={0}
                          onClick={open}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              open();
                            }
                          }}
                          title={isChat ? "Open this chat" : "Open this note"}
                        >
                          <span className="brain-cite-n">[{c.n}]</span>
                          <span className="brain-cite-src">{c.source}</span>
                          <span className="brain-cite-ref">{c.reference}</span>
                          {c.stale && (
                            <span
                              className="badge"
                              title="This source changed since it was indexed — reindex to refresh"
                            >
                              stale
                            </span>
                          )}
                          <span className="muted brain-cite-score">{c.score.toFixed(2)}</span>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            )}
          </div>
        )}

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
            <div className="brain-dedup">
              <div className="brain-ask-bar">
                <div className="muted" style={{ flex: 1 }}>
                  Near-duplicate memories — likely copy/paste or repeated-save notes worth merging.
                </div>
                <button onClick={() => void runDedupScan()} disabled={dupLoading}>
                  {dupLoading ? "Scanning…" : "Find duplicates"}
                </button>
              </div>
              {dupError && <div className="settings-err">{dupError}</div>}
              {dupGroups && dupGroups.length === 0 && !dupError && (
                <div className="muted">No near-duplicate memories found.</div>
              )}
              {dupGroups && dupGroups.length > 0 && (
                <div className="brain-list">
                  {dupGroups.map((g, gi) => (
                    <div key={gi} className="brain-row">
                      <div className="brain-row-head">
                        <strong>{g.members.length} similar notes</strong>
                        <span className="muted">{(g.max_similarity * 100).toFixed(0)}% match</span>
                      </div>
                      {g.members.map((m) => (
                        <div
                          key={m.open_path}
                          className="brain-row-interactive brain-cite"
                          role="button"
                          tabIndex={0}
                          onClick={() => {
                            useCortexStore.getState().setActivityTab("editor");
                            setTimeout(() => {
                              window.dispatchEvent(
                                new CustomEvent("cortex:editor-open", { detail: { path: m.open_path } }),
                              );
                            }, 0);
                          }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              useCortexStore.getState().setActivityTab("editor");
                              setTimeout(() => {
                                window.dispatchEvent(
                                  new CustomEvent("cortex:editor-open", { detail: { path: m.open_path } }),
                                );
                              }, 0);
                            }
                          }}
                          title="Open this note"
                        >
                          <span className="brain-cite-ref">{m.reference}</span>
                        </div>
                      ))}
                    </div>
                  ))}
                </div>
              )}
            </div>
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
