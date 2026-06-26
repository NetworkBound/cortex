/**
 * Snippets panel — manage saved `#snippet:name` reusable prompts.
 *
 * Left column: list of snippet names with last-used relative time.
 * Right column: editor with name input + monospace body textarea.
 *
 * Backed by `~/.cortex/snippets.json` through `src/lib/snippets.ts`. All
 * mutations roundtrip to the Tauri backend (`save_snippet`/`delete_snippet`)
 * and we reload the list on success so timestamps stay accurate.
 *
 * Dirty-tracking: an edit is "dirty" when either the name or body differs
 * from the loaded snapshot. New (unsaved) snippets are always dirty until
 * first save. Save is disabled until dirty + name is non-empty.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { PanelLoading } from "./Skeleton";
import {
  deleteSnippet,
  listSnippets,
  saveSnippet,
  type Snippet,
} from "@/lib/snippets";
import { pushToast } from "@/lib/toast";

/** Sentinel id for an in-memory "new snippet" draft before first save. */
const DRAFT_ID = "__draft__";

interface Draft {
  /** Original name on disk, or null for an unsaved draft. */
  origName: string | null;
  name: string;
  body: string;
}

export function SnippetsPanel() {
  const [snippets, setSnippets] = useState<Snippet[] | null>(null);
  const [activeName, setActiveName] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [saving, setSaving] = useState(false);

  const reload = useCallback(async () => {
    const list = await listSnippets();
    // Sort by last-used desc so freshly-used snippets bubble to the top.
    list.sort((a, b) => b.last_used_unix_ms - a.last_used_unix_ms);
    setSnippets(list);
    setActiveName((prev) => {
      if (prev === DRAFT_ID) return prev;
      if (prev && list.some((s) => s.name === prev)) return prev;
      return list[0]?.name ?? null;
    });
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Sync the editor draft to whichever snippet is currently selected. When
  // the active selection is the DRAFT_ID sentinel we keep whatever the user
  // has typed so the "New snippet" flow doesn't reset on every reload.
  useEffect(() => {
    if (activeName === DRAFT_ID) return;
    if (!snippets || activeName === null) {
      setDraft(null);
      return;
    }
    const s = snippets.find((x) => x.name === activeName);
    if (!s) {
      setDraft(null);
      return;
    }
    setDraft({ origName: s.name, name: s.name, body: s.body });
  }, [activeName, snippets]);

  const dirty = useMemo(() => {
    if (!draft) return false;
    if (draft.origName === null) return true; // unsaved draft
    const orig = snippets?.find((s) => s.name === draft.origName);
    if (!orig) return true;
    return draft.name !== orig.name || draft.body !== orig.body;
  }, [draft, snippets]);

  const nameValid = useMemo(() => {
    if (!draft) return false;
    const n = draft.name.trim();
    if (!n) return false;
    // Mirror the backend regex used by `expandSnippets`.
    return /^[A-Za-z0-9_\-.]+$/.test(n);
  }, [draft]);

  function startNew() {
    setActiveName(DRAFT_ID);
    setDraft({ origName: null, name: "", body: "" });
  }

  async function handleSave() {
    if (!draft || !nameValid) return;
    setSaving(true);
    try {
      // Renaming: write under the new name, then delete the old entry so we
      // don't end up with orphaned copies. Backend has no rename op.
      const saved = await saveSnippet(draft.name.trim(), draft.body);
      if (!saved) {
        pushToast({ title: "Save failed", body: "Backend rejected snippet.", kind: "error" });
        return;
      }
      if (draft.origName && draft.origName !== saved.name) {
        // If removing the old entry fails we'd silently leave an orphaned
        // duplicate. Surface that instead of claiming a clean rename.
        try {
          const removed = await deleteSnippet(draft.origName);
          if (!removed) {
            pushToast({
              title: "Renamed, but old copy remains",
              body: `Could not delete "${draft.origName}".`,
              kind: "error",
            });
          }
        } catch {
          pushToast({
            title: "Renamed, but old copy remains",
            body: `Could not delete "${draft.origName}".`,
            kind: "error",
          });
        }
      }
      pushToast({ title: "Saved", body: `#snippet:${saved.name}`, kind: "success" });
      setActiveName(saved.name);
      await reload();
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(name: string) {
    if (!(await confirmDialog({
      title: "Delete snippet?",
      message: `"${name}" will be deleted. This cannot be undone.`,
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    const ok = await deleteSnippet(name);
    if (!ok) {
      pushToast({ title: "Delete failed", body: name, kind: "error" });
      return;
    }
    pushToast({ title: "Deleted", body: name, kind: "info" });
    if (activeName === name) setActiveName(null);
    await reload();
  }

  async function handleCopy(s: Snippet) {
    try {
      await navigator.clipboard.writeText(s.body);
      pushToast({ title: "Copied", body: `#snippet:${s.name}`, kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }

  if (snippets === null) {
    return <PanelLoading label="Loading snippets" />;
  }

  const showingDraft = activeName === DRAFT_ID;
  const hasAny = snippets.length > 0 || showingDraft;

  return (
    <div className="skills-panel">
      <div className="skills-list">
        <div className="skills-list-head">
          <span className="muted">
            {snippets.length} snippet{snippets.length === 1 ? "" : "s"}
          </span>
          <div className="skills-list-head-actions">
            <button type="button" className="panel-head-action" onClick={startNew}>
              + new
            </button>
            <button type="button" className="panel-head-action ghost" onClick={() => void reload()}>
              Refresh
            </button>
          </div>
        </div>
        {showingDraft && (
          <div className="skills-row active snippets-row-draft">
            <div className="skills-row-name">
              {draft?.name.trim() || <span className="muted">(unsaved)</span>}
            </div>
            <div className="skills-row-desc muted">new — unsaved</div>
          </div>
        )}
        {snippets.map((s) => (
          <div
            key={s.name}
            className={`skills-row snippets-row ${
              activeName === s.name ? "active" : ""
            }`}
          >
            <button
              type="button"
              className="snippets-row-main"
              onClick={() => setActiveName(s.name)}
            >
              <div className="skills-row-name">{s.name}</div>
              <div className="skills-row-desc muted">
                {s.last_used_unix_ms > 0
                  ? `used ${timeAgo(s.last_used_unix_ms)}`
                  : "never used"}
              </div>
            </button>
            <div className="snippets-row-actions">
              <button
                type="button"
                className="link-btn"
                title="Copy body to clipboard"
                onClick={() => void handleCopy(s)}
              >
                Copy
              </button>
              <button
                type="button"
                className="link-btn danger"
                title="Delete snippet"
                onClick={() => void handleDelete(s.name)}
              >
                ×
              </button>
            </div>
          </div>
        ))}
      </div>
      <div className="skills-detail">
        {!hasAny && (
          <div className="muted skills-empty">
            No snippets yet. Use <code>#snippet:name</code> in chat to save a
            reusable prompt.
            <div className="skills-list-head-actions" style={{ marginTop: "var(--space-3)", justifyContent: "center" }}>
              <button type="button" className="panel-head-action" onClick={startNew}>
                + new snippet
              </button>
            </div>
          </div>
        )}
        {hasAny && !draft && (
          <div className="muted skills-empty">Select a snippet, or create a new one.</div>
        )}
        {draft && (
          <div className="snippets-editor">
            <label className="skills-runner-field">
              <span className="skills-runner-label">name</span>
              <input
                type="text"
                value={draft.name}
                placeholder="e.g. bugfix-checklist"
                onChange={(e) =>
                  setDraft((d) => (d ? { ...d, name: e.target.value } : d))
                }
              />
              {!nameValid && draft.name.length > 0 && (
                <span className="snippets-name-hint">
                  Letters, numbers, <code>_ - .</code> only.
                </span>
              )}
            </label>
            <label className="skills-runner-field snippets-body-field">
              <span className="skills-runner-label">body</span>
              <textarea
                className="snippets-body"
                value={draft.body}
                rows={16}
                placeholder="Reusable prompt text. Insert with #snippet:name in chat."
                onChange={(e) =>
                  setDraft((d) => (d ? { ...d, body: e.target.value } : d))
                }
              />
            </label>
            <div className="skills-runner-actions">
              <button
                type="button"
                className="btn-primary"
                disabled={!dirty || !nameValid || saving}
                onClick={() => void handleSave()}
              >
                {saving ? "Saving…" : "Save"}
              </button>
              {dirty && <span className="muted snippets-dirty">unsaved changes</span>}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

