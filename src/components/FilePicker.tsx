/**
 * Cursor-style @-vocabulary picker. Backwards compatible with the original
 * `FilePicker` (still triggered by `@` in chat, same props).
 *
 * WIRING NOTES (for ChatPane.tsx — DO NOT TOUCH HERE, comments only):
 *   - The component still exposes `{ open, query, onPick, onClose }` and calls
 *     `onPick(value)` with a STRING. For the legacy `files` kind, `value` is
 *     just the filename (existing behavior). For every other kind it is a
 *     `<kind>:<value>` envelope (see src/lib/at-vocab.ts).
 *   - ChatPane.tsx's `insertFile(name)` can keep inserting the literal string
 *     as today; nothing breaks. When you're ready, parse the envelope:
 *         const m = name.match(/^(files|folders|git|recent|docs|memory):(.+)$/);
 *         if (m) { const [, kind, value] = m; ... }
 *     and dispatch into the context-builder accordingly.
 *   - The picker auto-switches active kind when the user types a keyword like
 *     `@Folders/run` — no changes needed in ChatPane; just feed the raw
 *     `query` (everything after `@`) like before.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useCortexStore } from "@/state/store";
import {
  fetchVocab,
  detectVocab,
  stripVocabPrefix,
  VOCAB_KINDS,
  type VocabEntry,
  type VocabKind,
} from "@/lib/at-vocab";
import { VocabIcon } from "@/lib/vocab-icons";

interface Props {
  open: boolean;
  query: string;
  onPick: (filename: string) => void;
  onClose: () => void;
}

export function FilePicker({ open, query, onPick, onClose }: Props) {
  const active = useCortexStore((s) => s.activeProject);
  const [manualKind, setManualKind] = useState<VocabKind | null>(null);
  const [entries, setEntries] = useState<VocabEntry[]>([]);
  const [idx, setIdx] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);

  // Auto-detect kind from query prefix (e.g. "Folders/run" → folders).
  // A manual chip click overrides auto-detect until the user clears it.
  const autoKind = useMemo<VocabKind>(() => detectVocab(query), [query]);
  const activeKind: VocabKind = manualKind ?? autoKind;

  // The query passed to the backend strips the keyword if it triggered the kind.
  const effectiveQuery = useMemo(() => {
    if (manualKind) return query;
    return stripVocabPrefix(query);
  }, [query, manualKind]);

  // Reset manual kind when picker closes so next open starts fresh.
  useEffect(() => {
    if (!open) setManualKind(null);
  }, [open]);

  // Fetch entries when kind, query, or active project changes.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const root = active?.root ?? null;
    fetchVocab(activeKind, effectiveQuery, root)
      .then((res) => { if (!cancelled) setEntries(res); })
      .catch(() => { if (!cancelled) setEntries([]); });
    return () => { cancelled = true; };
  }, [open, activeKind, effectiveQuery, active]);

  // Reset selection when entries change.
  useEffect(() => { setIdx(0); }, [activeKind, effectiveQuery, open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); onClose(); }
      else if (e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, Math.max(entries.length - 1, 0)));
      }
      else if (e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      }
      else if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        const entry = entries[idx];
        if (entry) onPick(entry.value);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, entries, idx, onPick, onClose]);

  if (!open) return null;

  const emptyMsg = active
    ? "no matches"
    : activeKind === "recent" || activeKind === "docs"
      ? "nothing yet"
      : "pick a project first";

  return (
    <div ref={containerRef} className="file-picker">
      <div className="file-picker-chips" role="tablist">
        {VOCAB_KINDS.map((k) => (
          <button
            key={k.kind}
            type="button"
            role="tab"
            aria-selected={activeKind === k.kind}
            className={`file-picker-chip ${activeKind === k.kind ? "active" : ""}`}
            onMouseDown={(e) => { e.preventDefault(); setManualKind(k.kind); }}
            title={k.hint}
          >
            <VocabIcon kind={k.kind} size={13} />
            <span>{k.label}</span>
          </button>
        ))}
      </div>
      <div className="file-picker-head">
        <span className="muted">
          {activeKind} {effectiveQuery ? `· "${effectiveQuery}"` : ""}
          {active ? ` · ${active.name}` : ""}
        </span>
      </div>
      <ul>
        {entries.length === 0 && (
          <li className="muted">{emptyMsg}</li>
        )}
        {entries.map((entry, i) => (
          <li
            key={`${entry.kind}:${entry.value}:${i}`}
            className={i === idx ? "active" : ""}
            onMouseEnter={() => setIdx(i)}
            onClick={() => onPick(entry.value)}
          >
            <span className="file-picker-icon"><VocabIcon kind={entry.kind} /></span>
            <span className="file-picker-label">{entry.label}</span>
            {entry.preview && (
              <span className="file-picker-preview muted">{entry.preview}</span>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
