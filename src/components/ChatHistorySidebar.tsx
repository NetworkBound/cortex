import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { Star, SquarePen } from "lucide-react";
import { Chevron } from "@/lib/chevron";
import {
  getClaudeChat,
  listClaudeChats,
  truncateMessage,
  type ChatSummary,
  type ChatTranscript,
} from "@/lib/chat-history";
import {
  EMPTY_CHAT_META,
  listChatMeta,
  parseTagsInput,
  setChatMeta,
  type ChatMeta,
} from "@/lib/chat-meta";
import { ChatHistoryViewer } from "@/components/ChatHistoryViewer";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { pushToast } from "@/lib/toast";
import { promptDialog } from "@/lib/dialogs";

/**
 * Right-tab sidebar listing every parsed Claude/Cortex Gateway chat session,
 * grouped by project.
 *
 * Per-row actions:
 *   • Hover (300ms debounce) → fetch the first 6 turns via `getClaudeChat`
 *     and render a floating preview card showing the opening 3 user/asst
 *     turns. Results cache by `file_path` so we don't re-fetch on re-hover.
 *   • "Resume in chat" → fires the existing `cortex:chat-replay` event
 *     with the chat's first user message (wired in ChatPane).
 *   • "View" → opens <ChatHistoryViewer/> portal modal.
 *   • ⭐ → toggles favorite via `setChatMeta`, pinning the row to the top
 *     of its project group.
 *   • ✎ → inline rename input (Enter saves, Esc cancels). The override
 *     replaces the first-message snippet for display.
 *   • Right-click → context menu with "Add tags…" → comma-separated tags
 *     prompt, rendered as chips next to the title.
 *
 * Per-chat metadata is stored at `~/.cortex/chat-meta.json` and fetched
 * once on mount via `listChatMeta`; mutations update local state
 * optimistically.
 */

const HOVER_DELAY_MS = 300;
const PREVIEW_TURNS = 6; // matches the prior in-sidebar API call cap
const PREVIEW_RENDER_LIMIT = 3; // turns shown in the hover card
const AUTO_COLLAPSE_THRESHOLD = 10;

interface ProjectGroup {
  project: string;
  chats: ChatSummary[];
}

interface PreviewCache {
  [filePath: string]: ChatTranscript | "loading" | "error";
}

type MetaMap = Record<string, ChatMeta>;

export function ChatHistorySidebar() {
  const [chats, setChats] = useState<ChatSummary[]>([]);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // Project name → collapsed (true means hidden).
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // Track whether the user has manually toggled a group; we only apply the
  // > 10 auto-collapse heuristic to untouched groups.
  const [userToggled, setUserToggled] = useState<Set<string>>(new Set());
  const [previewCache, setPreviewCache] = useState<PreviewCache>({});
  const [hoveredPath, setHoveredPath] = useState<string | null>(null);
  const [viewerChat, setViewerChat] = useState<ChatSummary | null>(null);
  const [metaMap, setMetaMap] = useState<MetaMap>({});
  const [editingPath, setEditingPath] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState("");
  const hoverTimer = useRef<number | null>(null);

  useEffect(() => {
    let mounted = true;
    setLoading(true);
    Promise.all([listClaudeChats(), listChatMeta().catch(() => ({}))])
      .then(([c, m]) => {
        if (!mounted) return;
        setChats(c);
        setMetaMap(m as MetaMap);
        setError(null);
      })
      .catch((e) => {
        if (mounted) setError(humanizeError(e));
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
    };
  }, []);

  const getMeta = useCallback(
    (path: string): ChatMeta => metaMap[path] ?? EMPTY_CHAT_META,
    [metaMap],
  );

  const persistMeta = useCallback(async (path: string, next: ChatMeta) => {
    setMetaMap((prev) => ({ ...prev, [path]: next }));
    try {
      await setChatMeta(path, next);
    } catch (e) {
      console.warn("chat-meta save failed", e);
    }
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return chats;
    return chats.filter((c) => {
      const proj = (c.project ?? "").toLowerCase();
      const first = (c.first_message ?? "").toLowerCase();
      const meta = getMeta(c.file_path);
      const title = (meta.custom_title ?? "").toLowerCase();
      const tagHit = meta.tags.some((t) => t.toLowerCase().includes(q));
      return proj.includes(q) || first.includes(q) || title.includes(q) || tagHit;
    });
  }, [chats, query, getMeta]);

  const groups: ProjectGroup[] = useMemo(() => {
    const map = new Map<string, ChatSummary[]>();
    for (const c of filtered) {
      const key = c.project ?? "(global history)";
      const arr = map.get(key) ?? [];
      arr.push(c);
      map.set(key, arr);
    }
    const out: ProjectGroup[] = [];
    for (const [project, list] of map) {
      // Favorites first within each group, then recency.
      list.sort((a, b) => {
        const af = getMeta(a.file_path).is_favorite ? 1 : 0;
        const bf = getMeta(b.file_path).is_favorite ? 1 : 0;
        if (af !== bf) return bf - af;
        return b.modified_unix_ms - a.modified_unix_ms;
      });
      out.push({ project, chats: list });
    }
    out.sort(
      (a, b) =>
        (b.chats[0]?.modified_unix_ms ?? 0) -
        (a.chats[0]?.modified_unix_ms ?? 0),
    );
    return out;
  }, [filtered, getMeta]);

  // Auto-collapse groups with > 10 chats on first render only (per group).
  useEffect(() => {
    setCollapsed((cur) => {
      const next = new Set(cur);
      for (const g of groups) {
        if (userToggled.has(g.project)) continue;
        if (g.chats.length > AUTO_COLLAPSE_THRESHOLD) next.add(g.project);
      }
      return next;
    });
  }, [groups, userToggled]);

  const toggle = (project: string) => {
    setUserToggled((cur) => {
      if (cur.has(project)) return cur;
      const next = new Set(cur);
      next.add(project);
      return next;
    });
    setCollapsed((cur) => {
      const next = new Set(cur);
      if (next.has(project)) next.delete(project);
      else next.add(project);
      return next;
    });
  };

  const loadPreview = useCallback(
    async (path: string) => {
      if (previewCache[path] !== undefined) return;
      setPreviewCache((p) => ({ ...p, [path]: "loading" }));
      try {
        const t = await getClaudeChat(path, PREVIEW_TURNS);
        setPreviewCache((p) => ({ ...p, [path]: t }));
      } catch {
        setPreviewCache((p) => ({ ...p, [path]: "error" }));
      }
    },
    [previewCache],
  );

  const onRowEnter = (path: string) => {
    if (hoverTimer.current) window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => {
      setHoveredPath(path);
      void loadPreview(path);
    }, HOVER_DELAY_MS);
  };

  const onRowLeave = () => {
    if (hoverTimer.current) {
      window.clearTimeout(hoverTimer.current);
      hoverTimer.current = null;
    }
    setHoveredPath(null);
  };

  useEffect(() => {
    return () => {
      if (hoverTimer.current) window.clearTimeout(hoverTimer.current);
    };
  }, []);

  // Trigger the ChatGPT export importer — opens a file picker for the
  // user's `conversations.json` (from the OpenAI data-export ZIP),
  // hands it to the Rust importer, and reloads the chat list so the
  // imported threads appear under a `chatgpt-import` group.
  const importChatgpt = async () => {
    try {
      const selected = await openDialog({
        multiple: false,
        filters: [{ name: "ChatGPT export", extensions: ["json"] }],
      });
      if (!selected || typeof selected !== "string") return;
      pushToast({ title: "Importing ChatGPT export…", kind: "info" });
      const result = await invoke<{ imported: number; skipped: number; out_dir: string }>(
        "import_chatgpt_export",
        { path: selected },
      );
      pushToast({
        title: `ChatGPT import complete`,
        body: `${result.imported} new, ${result.skipped} skipped`,
        kind: "success",
      });
      // Refetch chats so the new threads appear.
      const chats = await listClaudeChats();
      setChats(chats);
    } catch (e) {
      pushToast({ title: "ChatGPT import failed", body: humanizeError(e), kind: "error" });
    }
  };

  const resumeChat = async (chat: ChatSummary) => {
    let message = chat.first_message ?? "";
    const cached = previewCache[chat.file_path];
    if (cached && cached !== "loading" && cached !== "error") {
      const firstUser = cached.turns.find((tn) => tn.role === "user");
      if (firstUser) message = firstUser.content;
    } else {
      try {
        const t = await getClaudeChat(chat.file_path, 20);
        const firstUser = t.turns.find((tn) => tn.role === "user");
        if (firstUser) message = firstUser.content;
      } catch {
        /* fall back to summary's first_message */
      }
    }
    window.dispatchEvent(
      new CustomEvent("cortex:chat-replay", {
        detail: {
          message,
          session_id: chat.session_id,
          file_path: chat.file_path,
        },
      }),
    );
  };

  const toggleFavorite = (chat: ChatSummary) => {
    const current = getMeta(chat.file_path);
    void persistMeta(chat.file_path, {
      ...current,
      is_favorite: !current.is_favorite,
    });
  };

  const beginRename = (chat: ChatSummary) => {
    const current = getMeta(chat.file_path);
    const seed =
      current.custom_title ??
      (chat.first_message
        ? truncateMessage(chat.first_message, 80)
        : `session ${chat.session_id.slice(-10)}`);
    setEditingPath(chat.file_path);
    setEditingDraft(seed);
  };

  const cancelRename = () => {
    setEditingPath(null);
    setEditingDraft("");
  };

  const commitRename = (chat: ChatSummary) => {
    const next = editingDraft.trim();
    const current = getMeta(chat.file_path);
    void persistMeta(chat.file_path, {
      ...current,
      custom_title: next.length > 0 ? next : null,
    });
    cancelRename();
  };

  const editTags = async (chat: ChatSummary) => {
    const current = getMeta(chat.file_path);
    const seed = current.tags.join(", ");
    // A richer chip editor can land alongside the BookmarksPanel refactor
    // later; for now a plain prompt keeps the sidebar lean.
    const raw = await promptDialog({ title: "Edit tags", message: "Tags (comma-separated)", initialValue: seed });
    if (raw === null) return;
    void persistMeta(chat.file_path, {
      ...current,
      tags: parseTagsInput(raw),
    });
  };

  const renderPreview = (path: string) => {
    const entry = previewCache[path];
    if (!entry) return null;
    if (entry === "loading") {
      return <div className="chat-history-preview-loading">loading…</div>;
    }
    if (entry === "error") {
      return <div className="chat-history-preview-loading">preview unavailable</div>;
    }
    const turns = entry.turns
      .filter((t) => t.role === "user" || t.role === "assistant")
      .slice(0, PREVIEW_RENDER_LIMIT);
    if (turns.length === 0) {
      return <div className="chat-history-preview-loading">empty transcript</div>;
    }
    return (
      <div className="chat-history-preview-turns">
        {turns.map((t, i) => (
          <div
            key={i}
            className={`chat-history-preview-turn chat-history-preview-turn-${t.role}`}
          >
            <span className="chat-history-preview-role">
              {t.role === "user" ? "you" : "claude"}
            </span>
            <span className="chat-history-preview-text">
              {truncateMessage(t.content, 220)}
            </span>
          </div>
        ))}
      </div>
    );
  };

  return (
    <div className="chat-history-sidebar">
      <div className="chat-history-search">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter by project, message, title, or tag…"
        />
        {query && (
          <button className="link-btn" onClick={() => setQuery("")}>
            Clear
          </button>
        )}
        <button
          className="link-btn"
          onClick={() => void importChatgpt()}
          title="Import a ChatGPT data export (conversations.json) into your chat history"
        >
          + ChatGPT
        </button>
      </div>
      {error && <div className="chat-history-error">{error}</div>}
      {loading && <div className="chat-history-empty">loading…</div>}
      {!loading && groups.length === 0 && (
        <div className="chat-history-empty">
          {query ? `No chats match "${query}".` : "No Claude chats found yet."}
        </div>
      )}
      <div className="chat-history-body">
        {groups.map((g) => {
          const open = !collapsed.has(g.project);
          return (
            <div key={g.project} className="chat-history-group">
              <button
                className="chat-history-group-head"
                onClick={() => toggle(g.project)}
                title={open ? "collapse" : "expand"}
              >
                <span className="chat-history-caret"><Chevron open={open} size={13} /></span>
                <strong>{g.project}</strong>
                <span className="muted">{g.chats.length} chats</span>
              </button>
              {open && (
                <div className="chat-history-list">
                  {g.chats.map((c) => {
                    const isHovered = hoveredPath === c.file_path;
                    const meta = getMeta(c.file_path);
                    const isEditing = editingPath === c.file_path;
                    const displayTitle = meta.custom_title
                      ? meta.custom_title
                      : c.first_message
                        ? truncateMessage(c.first_message, 80)
                        : `session ${c.session_id.slice(-10)}`;
                    return (
                      <div
                        key={c.file_path}
                        className={`chat-history-row${meta.is_favorite ? " chat-history-row-fav" : ""}`}
                        role="button"
                        tabIndex={0}
                        onMouseEnter={() => onRowEnter(c.file_path)}
                        onMouseLeave={onRowLeave}
                        onClick={() => { if (!isEditing) void resumeChat(c); }}
                        onKeyDown={(e) => {
                          if (isEditing) return;
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            void resumeChat(c);
                          }
                        }}
                        onContextMenu={(e) => {
                          e.preventDefault();
                          void editTags(c);
                        }}
                        title={c.file_path}
                      >
                        <div className="chat-history-row-head">
                          <button
                            className={`chat-history-star${meta.is_favorite ? " is-on" : ""}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              toggleFavorite(c);
                            }}
                            title={
                              meta.is_favorite
                                ? "unfavorite"
                                : "favorite (pin to top)"
                            }
                            aria-pressed={meta.is_favorite}
                          >
                            <Star
                              size={14}
                              strokeWidth={1.75}
                              fill={meta.is_favorite ? "currentColor" : "none"}
                              aria-hidden="true"
                            />
                          </button>
                          {isEditing ? (
                            <input
                              className="chat-history-rename"
                              autoFocus
                              value={editingDraft}
                              onChange={(e) => setEditingDraft(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") { e.preventDefault(); commitRename(c); }
                                else if (e.key === "Escape") { e.preventDefault(); cancelRename(); }
                              }}
                              onBlur={() => commitRename(c)}
                              onClick={(e) => e.stopPropagation()}
                              placeholder="Custom title…"
                            />
                          ) : (
                            <>
                              <span className="chat-history-title">{displayTitle}</span>
                              <button
                                className="chat-history-rename-btn"
                                onClick={(e) => { e.stopPropagation(); beginRename(c); }}
                                title="rename"
                                aria-label="rename chat"
                              >
                                <SquarePen size={14} strokeWidth={1.75} aria-hidden="true" />
                              </button>
                            </>
                          )}
                          <span className="muted">
                            {timeAgo(c.modified_unix_ms)}
                          </span>
                        </div>
                        <div className="chat-history-meta">
                          {c.message_count} msgs · {c.session_id.slice(-8)}
                        </div>
                        {meta.tags.length > 0 && (
                          <div className="chat-history-tags">
                            {meta.tags.map((t) => (
                              <span key={t} className="chat-history-tag">
                                {t}
                              </span>
                            ))}
                          </div>
                        )}
                        <div className="chat-history-row-actions">
                          <button
                            className="link-btn"
                            onClick={(e) => {
                              e.stopPropagation();
                              void resumeChat(c);
                            }}
                          >
                            Resume in chat
                          </button>
                          <button
                            className="link-btn"
                            onClick={(e) => {
                              e.stopPropagation();
                              setViewerChat(c);
                            }}
                          >
                            View
                          </button>
                          <button
                            className="link-btn"
                            onClick={(e) => {
                              e.stopPropagation();
                              void editTags(c);
                            }}
                          >
                            Tags…
                          </button>
                        </div>
                        {isHovered && (
                          <div className="chat-history-preview-card">
                            {renderPreview(c.file_path)}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>
      {viewerChat && (
        <ChatHistoryViewer
          chat={viewerChat}
          onClose={() => setViewerChat(null)}
        />
      )}
    </div>
  );
}
