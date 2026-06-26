/**
 * Bookmarks / favorites panel — quick-access list of pinned artefacts.
 *
 * Layout: filter chip strip + search input on top, a grouped-by-kind list
 * below, and a collapsible "+ Add bookmark" form pinned to the bottom of
 * the list column. Clicking a row dispatches to `openBookmark`, which
 * picks the right surface per kind (editor / trace viewer / chat resume /
 * shell / toast).
 *
 * Storage lives at `~/.cortex/bookmarks.json` via `src/lib/bookmarks.ts`.
 * Every mutation re-fetches the list so display order (most-recently-opened
 * → most-recently-created) stays in sync with the backend's sort key.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Star } from "lucide-react";
import { BookmarkIcon } from "@/lib/bookmark-icons";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";
import {
  BOOKMARK_CHANGED_EVENT,
  BOOKMARK_KINDS,
  BOOKMARK_KIND_LABELS,
  addBookmark,
  deleteBookmark,
  listBookmarks,
  openBookmark,
  parseTags,
  timeAgo,
  type Bookmark,
  type BookmarkKind,
} from "@/lib/bookmarks";
import { pushToast } from "@/lib/toast";

/** Initial state for the "Add bookmark" form. Kept at module scope so the
 *  reset path can reuse it without re-allocating per render. */
const EMPTY_DRAFT: DraftState = {
  kind: "note",
  label: "",
  target: "",
  tagsRaw: "",
  note: "",
};

interface DraftState {
  kind: BookmarkKind;
  label: string;
  target: string;
  tagsRaw: string;
  note: string;
}

export function BookmarksPanel() {
  const [items, setItems] = useState<Bookmark[] | null>(null);
  const [filterKind, setFilterKind] = useState<BookmarkKind | "all">("all");
  const [query, setQuery] = useState("");
  const [showAdd, setShowAdd] = useState(false);
  const [draft, setDraft] = useState<DraftState>(EMPTY_DRAFT);
  const [saving, setSaving] = useState(false);
  // Distinguishes "couldn't reach the backend" from a genuinely empty list so a
  // down backend doesn't read as "no bookmarks yet" (and so the panel doesn't
  // hang on the loading skeleton forever when the fetch rejects).
  const [loadError, setLoadError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const list = await listBookmarks();
      setItems(list);
      setLoadError(null);
    } catch (e) {
      setLoadError(humanizeError(e));
      // Drop the loading skeleton even on failure so the error state can render.
      setItems((cur) => cur ?? []);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Stay in sync with mutations from anywhere else (slash command, etc).
  useEffect(() => {
    const onChange = () => void reload();
    window.addEventListener(BOOKMARK_CHANGED_EVENT, onChange);
    return () => window.removeEventListener(BOOKMARK_CHANGED_EVENT, onChange);
  }, [reload]);

  // Lowercase + tag-aware filter. Match against label, target, tags, and
  // note so users can find a bookmark by any visible attribute.
  const filtered = useMemo(() => {
    if (!items) return [];
    const q = query.trim().toLowerCase();
    return items.filter((b) => {
      if (filterKind !== "all" && b.kind !== filterKind) return false;
      if (!q) return true;
      if (b.label.toLowerCase().includes(q)) return true;
      if (b.target.toLowerCase().includes(q)) return true;
      if (b.note && b.note.toLowerCase().includes(q)) return true;
      return b.tags.some((t) => t.toLowerCase().includes(q));
    });
  }, [items, filterKind, query]);

  // Group by kind preserving the backend's recency sort within each bucket.
  const grouped = useMemo(() => {
    const buckets: Partial<Record<BookmarkKind, Bookmark[]>> = {};
    for (const b of filtered) {
      const arr = buckets[b.kind] ?? (buckets[b.kind] = []);
      arr.push(b);
    }
    return BOOKMARK_KINDS.flatMap<{ kind: BookmarkKind; rows: Bookmark[] }>(
      (k) => (buckets[k]?.length ? [{ kind: k, rows: buckets[k] ?? [] }] : []),
    );
  }, [filtered]);

  async function handleAdd() {
    const label = draft.label.trim();
    const target = draft.target.trim() || label;
    if (!label) {
      pushToast({
        title: "Add bookmark",
        body: "Label is required.",
        kind: "warning",
      });
      return;
    }
    setSaving(true);
    try {
      const saved = await addBookmark({
        kind: draft.kind,
        label,
        target,
        tags: parseTags(draft.tagsRaw),
        note: draft.note.trim() || null,
      });
      if (!saved) {
        pushToast({
          title: "Add failed",
          body: "Backend rejected bookmark.",
          kind: "error",
        });
        return;
      }
      pushToast({
        title: "Bookmarked",
        body: `${BOOKMARK_KIND_LABELS[saved.kind]} — ${saved.label}`,
        kind: "success",
      });
      setDraft(EMPTY_DRAFT);
      setShowAdd(false);
      await reload();
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(b: Bookmark) {
    if (!(await confirmDialog({
      title: "Remove bookmark?",
      message: `"${b.label}" will be removed from your bookmarks.`,
      confirmLabel: "Remove",
      danger: true,
    }))) return;
    const ok = await deleteBookmark(b.id);
    if (!ok) {
      pushToast({
        title: "Delete failed",
        body: b.label,
        kind: "error",
      });
      return;
    }
    await reload();
  }

  async function handleOpen(b: Bookmark) {
    try {
      await openBookmark(b);
    } catch (e) {
      pushToast({ title: "Couldn't open bookmark", body: humanizeError(e), kind: "error" });
      return;
    }
    // Best-effort reload so the row jumps to the top after touch-bookmark
    // bumps last_opened_unix_ms. We delay a tick so the backend has time to
    // commit the timestamp before we re-fetch.
    setTimeout(() => void reload(), 150);
  }

  if (items === null) {
    return <PanelLoading label="Loading bookmarks" />;
  }

  const total = items.length;

  return (
    <div className="bookmarks-panel">
      <div className="bookmarks-head">
        <div className="bookmarks-filters">
          <button
            type="button"
            className={`bookmarks-chip ${filterKind === "all" ? "active" : ""}`}
            onClick={() => setFilterKind("all")}
          >
            <Star size={13} strokeWidth={1.75} aria-hidden="true" /> all{" "}
            <span className="muted">{total}</span>
          </button>
          {BOOKMARK_KINDS.map((k) => {
            const count = items.filter((b) => b.kind === k).length;
            if (count === 0 && filterKind !== k) return null;
            return (
              <button
                key={k}
                type="button"
                className={`bookmarks-chip ${filterKind === k ? "active" : ""}`}
                onClick={() => setFilterKind(k)}
                title={BOOKMARK_KIND_LABELS[k]}
              >
                <BookmarkIcon kind={k} size={13} /> {k}{" "}
                <span className="muted">{count}</span>
              </button>
            );
          })}
        </div>
        <input
          type="search"
          className="bookmarks-search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search label, tag, target…"
        />
        <div className="bookmarks-toolbar">
          <button
            type="button"
            className="link-btn"
            onClick={() => setShowAdd((v) => !v)}
          >
            {showAdd ? "× Cancel" : "+ Add bookmark"}
          </button>
          <button type="button" className="link-btn" onClick={() => void reload()}>
            Refresh
          </button>
        </div>
      </div>

      {showAdd && (
        <div className="bookmarks-form">
          <label className="bookmarks-field">
            <span className="bookmarks-field-label">kind</span>
            <select
              value={draft.kind}
              onChange={(e) =>
                setDraft((d) => ({ ...d, kind: e.target.value as BookmarkKind }))
              }
            >
              {BOOKMARK_KINDS.map((k) => (
                <option key={k} value={k}>
                  {BOOKMARK_KIND_LABELS[k]}
                </option>
              ))}
            </select>
          </label>
          <label className="bookmarks-field">
            <span className="bookmarks-field-label">label</span>
            <input
              type="text"
              value={draft.label}
              placeholder="Quick title shown in the list"
              onChange={(e) => setDraft((d) => ({ ...d, label: e.target.value }))}
            />
          </label>
          <label className="bookmarks-field">
            <span className="bookmarks-field-label">target</span>
            <input
              type="text"
              value={draft.target}
              placeholder={targetPlaceholder(draft.kind)}
              onChange={(e) => setDraft((d) => ({ ...d, target: e.target.value }))}
            />
          </label>
          <label className="bookmarks-field">
            <span className="bookmarks-field-label">tags</span>
            <input
              type="text"
              value={draft.tagsRaw}
              placeholder="comma, separated"
              onChange={(e) =>
                setDraft((d) => ({ ...d, tagsRaw: e.target.value }))
              }
            />
          </label>
          <label className="bookmarks-field">
            <span className="bookmarks-field-label">note</span>
            <input
              type="text"
              value={draft.note}
              placeholder="Optional one-liner"
              onChange={(e) => setDraft((d) => ({ ...d, note: e.target.value }))}
            />
          </label>
          <div className="bookmarks-form-actions">
            <button
              type="button"
              className="btn-primary"
              disabled={saving || !draft.label.trim()}
              onClick={() => void handleAdd()}
            >
              {saving ? "Saving…" : "Save bookmark"}
            </button>
          </div>
        </div>
      )}

      <div className="bookmarks-list">
        {loadError && (
          <div className="bookmarks-empty" role="alert" style={{ color: "var(--danger)" }}>
            Couldn't load bookmarks. {loadError}{" "}
            <button type="button" className="link-btn" onClick={() => void reload()}>
              Retry
            </button>
          </div>
        )}
        {!loadError && total === 0 && (
          <div className="muted bookmarks-empty">
            No bookmarks yet. Click <strong>+ Add bookmark</strong> or use{" "}
            <code>/bookmark &lt;label&gt;</code> in chat.
          </div>
        )}
        {total > 0 && filtered.length === 0 && (
          <div className="muted bookmarks-empty">No matches.</div>
        )}
        {grouped.map(({ kind, rows }) => (
          <div key={kind} className="bookmarks-group">
            <div className="bookmarks-group-head">
              <BookmarkIcon kind={kind} size={13} />{" "}
              {BOOKMARK_KIND_LABELS[kind]}{" "}
              <span className="muted">({rows.length})</span>
            </div>
            {rows.map((b) => (
              <div key={b.id} className="bookmarks-row">
                <button
                  type="button"
                  className="bookmarks-row-main"
                  onClick={() => void handleOpen(b)}
                  title={b.target}
                >
                  <div className="bookmarks-row-label">{b.label}</div>
                  <div className="bookmarks-row-target muted">{b.target}</div>
                  {(b.tags.length > 0 || b.note) && (
                    <div className="bookmarks-row-meta">
                      {b.tags.map((t) => (
                        <span key={t} className="bookmarks-tag">
                          #{t}
                        </span>
                      ))}
                      {b.note && (
                        <span className="bookmarks-note muted">{b.note}</span>
                      )}
                    </div>
                  )}
                  <div className="bookmarks-row-stamp muted">
                    {b.last_opened_unix_ms
                      ? `opened ${timeAgo(b.last_opened_unix_ms)}`
                      : `added ${timeAgo(b.created_unix_ms)}`}
                  </div>
                </button>
                <button
                  type="button"
                  className="link-btn danger bookmarks-row-del"
                  title="Remove bookmark"
                  onClick={() => void handleDelete(b)}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function targetPlaceholder(kind: BookmarkKind): string {
  switch (kind) {
    case "memory":
      return "/path/to/memory.md";
    case "file":
      return "/absolute/or/project/path";
    case "trace":
      return "trace-ulid";
    case "session":
      return "session-id";
    case "url":
      return "https://example.com";
    case "note":
      return "Defaults to the label if blank";
  }
}
