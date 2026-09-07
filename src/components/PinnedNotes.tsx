/**
 * Pinned notes rail — small horizontal strip of chip-buttons rendered above
 * the textarea (Open WebUI-style attachment row). Each chip:
 *  - is a drag source so users can reorder by drag-and-drop
 *  - shows a preview popover on click
 *  - has an ✕ corner button to remove the pin
 *
 * The rail is intentionally lightweight — it doesn't know anything about
 * the chat send pipeline. Consumers grab the formatted prefix via
 * `formatForPrepend(listPinnedNotes())` from `lib/pinned-notes`.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  PINNED_NOTES_EVENT,
  listPinnedNotes,
  removePinnedNote,
  reorderPinnedNotes,
  type PinnedNote,
} from "@/lib/pinned-notes";

function bytesLabel(n: number): string {
  if (n < 1024) return `${n} B`;
  return `${(n / 1024).toFixed(1)} KiB`;
}

export function PinnedNotes() {
  const [notes, setNotes] = useState<PinnedNote[]>([]);
  const [previewId, setPreviewId] = useState<string | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const popoverRef = useRef<HTMLDivElement>(null);

  // Initial load + subscribe to mutations. The pinned-notes lib fires the
  // event after every write so we can stay in lockstep without polling.
  const refresh = useCallback(async () => {
    const fresh = await listPinnedNotes();
    setNotes(fresh);
  }, []);

  useEffect(() => {
    void refresh();
    const onChange = () => {
      void refresh();
    };
    window.addEventListener(PINNED_NOTES_EVENT, onChange);
    return () => window.removeEventListener(PINNED_NOTES_EVENT, onChange);
  }, [refresh]);

  // Close popover on outside click. Bound while a preview is open so we
  // don't pay the listener cost when the rail is idle.
  useEffect(() => {
    if (!previewId) return;
    const onDocClick = (e: MouseEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (popoverRef.current && !popoverRef.current.contains(target)) {
        setPreviewId(null);
      }
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [previewId]);

  if (notes.length === 0) return null;

  const handleDragStart = (id: string) => (e: React.DragEvent) => {
    setDragId(id);
    try {
      e.dataTransfer.setData("text/x-cortex-pin", id);
      e.dataTransfer.effectAllowed = "move";
    } catch {
      /* some browsers throw if you set data twice — ignore */
    }
  };

  const handleDragOver = (e: React.DragEvent) => {
    if (!dragId) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
  };

  const handleDrop = (targetId: string) => async (e: React.DragEvent) => {
    e.preventDefault();
    if (!dragId || dragId === targetId) {
      setDragId(null);
      return;
    }
    const targetIdx = notes.findIndex((n) => n.id === targetId);
    if (targetIdx < 0) {
      setDragId(null);
      return;
    }
    await reorderPinnedNotes(dragId, targetIdx);
    setDragId(null);
  };

  const handleRemove = async (id: string) => {
    setPreviewId((curr) => (curr === id ? null : curr));
    await removePinnedNote(id);
  };

  const preview = previewId ? notes.find((n) => n.id === previewId) : null;

  return (
    <div className="pinned-notes-rail" aria-label="Pinned notes">
      {notes.map((n) => (
        <div
          key={n.id}
          className={`pinned-note-chip${dragId === n.id ? " dragging" : ""}`}
          draggable
          onDragStart={handleDragStart(n.id)}
          onDragOver={handleDragOver}
          onDrop={handleDrop(n.id)}
          title={n.source_path ?? n.label}
        >
          <button
            type="button"
            className="pinned-note-chip-label"
            onClick={() =>
              setPreviewId((curr) => (curr === n.id ? null : n.id))
            }
            aria-haspopup="dialog"
            aria-expanded={previewId === n.id}
          >
            <span className="pinned-note-chip-icon" aria-hidden>
              📌
            </span>
            <span className="pinned-note-chip-text">{n.label}</span>
          </button>
          <button
            type="button"
            className="pinned-note-chip-remove"
            onClick={() => void handleRemove(n.id)}
            aria-label={`Remove pinned note ${n.label}`}
            title="Unpin"
          >
            ×
          </button>
        </div>
      ))}

      {preview && (
        <div
          ref={popoverRef}
          className="pinned-note-popover"
          role="dialog"
          aria-label={`Pinned note preview: ${preview.label}`}
        >
          <div className="pinned-note-popover-head">
            <strong>{preview.label}</strong>
            <span className="muted pinned-note-popover-meta">
              {bytesLabel(preview.content.length)}
              {preview.source_path ? ` · ${preview.source_path}` : ""}
            </span>
          </div>
          <pre className="pinned-note-popover-body">{preview.content}</pre>
        </div>
      )}
    </div>
  );
}
