/**
 * Custom slash builder — portal modal. Lists every user-defined slash from
 * `~/.cortex/custom-slashes.yaml`, lets the user create / edit / delete an
 * entry, and re-registers the live `COMMANDS` array on save so the new
 * command is dispatchable without a reload.
 *
 * Mirrors the SkillBuilderModal layout: zinc surface, amber accent,
 * `.modal-backdrop` + `.modal` containers. The component is self-mounting
 * (same pattern as KeyVaultPanel / IDEExportModal / DedupePanel) so we
 * don't touch App.tsx — `openCustomSlashBuilder()` spins up a detached
 * React root on `document.body` and tears it down on close.
 */
import { useEffect, useMemo, useState } from "react";
import { createRoot, type Root } from "react-dom/client";

import {
  deleteCustomSlash,
  loadCustomSlashes,
  pushCustomSlashes,
  saveCustomSlash,
  type CustomSlash,
} from "@/lib/custom-slashes";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";

const KEBAB_RE = /^[a-z0-9]+(?:[-_][a-z0-9]+)*$/;
const NAME_MAX = 48;
const DESC_MAX = 256;
const BODY_MAX = 16 * 1024;

interface DraftState {
  name: string;
  description: string;
  body: string;
}

const EMPTY_DRAFT: DraftState = { name: "", description: "", body: "" };

function lineCount(body: string): number {
  return body
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0 && !l.startsWith("#")).length;
}

interface CustomSlashBuilderProps {
  onClose: () => void;
}

function CustomSlashBuilder({ onClose }: CustomSlashBuilderProps) {
  const [items, setItems] = useState<CustomSlash[]>([]);
  const [loading, setLoading] = useState(true);
  const [draft, setDraft] = useState<DraftState>(EMPTY_DRAFT);
  /** When non-null, we're editing the slash with this name (rename allowed
   *  but the original is removed before the save). Null = new entry. */
  const [editingName, setEditingName] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Esc closes — matches every other modal in the codebase.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Initial fetch.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const all = await loadCustomSlashes();
      if (cancelled) return;
      setItems(all);
      setLoading(false);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const existingNames = useMemo(
    () => new Set(items.map((s) => s.name)),
    [items],
  );

  const trimmedName = draft.name.trim();
  const nameInvalid = trimmedName.length > 0 && !KEBAB_RE.test(trimmedName);
  const nameTooLong = trimmedName.length > NAME_MAX;
  const nameCollision =
    trimmedName.length > 0 &&
    trimmedName !== editingName &&
    existingNames.has(trimmedName);
  const bodyEmpty = draft.body.trim().length === 0;
  const bodyTooBig = draft.body.length > BODY_MAX;
  const descTooLong = draft.description.length > DESC_MAX;

  const canSave =
    trimmedName.length > 0 &&
    !nameInvalid &&
    !nameTooLong &&
    !nameCollision &&
    !bodyEmpty &&
    !bodyTooBig &&
    !descTooLong &&
    !busy;

  function resetDraft() {
    setDraft(EMPTY_DRAFT);
    setEditingName(null);
  }

  function startEdit(slash: CustomSlash) {
    setDraft({
      name: slash.name,
      description: slash.description,
      body: slash.body,
    });
    setEditingName(slash.name);
  }

  async function handleSave() {
    if (!canSave) return;
    setBusy(true);
    try {
      // Rename: if the user changed the name while editing, drop the old
      // file entry first so the upsert doesn't leave a stale row behind.
      if (editingName && editingName !== trimmedName) {
        await deleteCustomSlash(editingName);
      }
      const saved = await saveCustomSlash({
        name: trimmedName,
        description: draft.description.trim(),
        body: draft.body,
      });
      if (!saved) {
        pushToast({
          title: "Save failed",
          body: "Couldn't persist the custom slash — check the logs.",
          kind: "error",
        });
        return;
      }
      const fresh = await loadCustomSlashes();
      setItems(fresh);
      pushCustomSlashes(fresh);
      resetDraft();
      pushToast({
        title: "Custom slash saved",
        body: `/${saved.name} — ${lineCount(saved.body)} step(s)`,
        kind: "success",
      });
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(name: string) {
    if (!(await confirmDialog({
      title: "Delete custom slash?",
      message: `Delete /${name}?`,
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    setBusy(true);
    try {
      const ok = await deleteCustomSlash(name);
      if (!ok) {
        pushToast({
          title: "Delete failed",
          body: `Couldn't remove /${name}.`,
          kind: "error",
        });
        return;
      }
      const fresh = await loadCustomSlashes();
      setItems(fresh);
      pushCustomSlashes(fresh);
      if (editingName === name) resetDraft();
      pushToast({
        title: "Custom slash deleted",
        body: `/${name}`,
        kind: "success",
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="modal custom-slash-modal"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h2>Custom slash commands</h2>
        <p className="custom-slash-hint muted">
          Saved to <code>~/.cortex/custom-slashes.yaml</code>. Each line in the
          body is run as a separate slash command, in order.
        </p>

        <div className="custom-slash-list">
          {loading ? (
            <div className="muted">Loading…</div>
          ) : items.length === 0 ? (
            <div className="muted">No custom slashes yet — define one below.</div>
          ) : (
            items.map((slash) => (
              <div className="custom-slash-row" key={slash.name}>
                <div className="custom-slash-row-main">
                  <div className="custom-slash-row-name">/{slash.name}</div>
                  {slash.description && (
                    <div className="custom-slash-row-desc muted">
                      {slash.description}
                    </div>
                  )}
                  <div className="custom-slash-row-meta muted">
                    {lineCount(slash.body)} step
                    {lineCount(slash.body) === 1 ? "" : "s"}
                  </div>
                </div>
                <div className="custom-slash-row-actions">
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => startEdit(slash)}
                    disabled={busy}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className="link-btn custom-slash-row-delete"
                    onClick={() => void handleDelete(slash.name)}
                    disabled={busy}
                  >
                    Delete
                  </button>
                </div>
              </div>
            ))
          )}
        </div>

        <div className="custom-slash-form">
          <div className="custom-slash-form-head">
            <span>{editingName ? `Editing /${editingName}` : "New custom slash"}</span>
            {editingName && (
              <button
                type="button"
                className="link-btn"
                onClick={resetDraft}
                disabled={busy}
              >
                Cancel edit
              </button>
            )}
          </div>

          <label>
            <span>name</span>
            <input
              type="text"
              placeholder="kebab-case (e.g. morning, clean-build)"
              value={draft.name}
              onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
              autoFocus
            />
            {nameInvalid && (
              <span className="custom-slash-warn">
                Use lowercase letters, digits, and single - or _ separators.
              </span>
            )}
            {nameTooLong && (
              <span className="custom-slash-warn">
                Name is too long (max {NAME_MAX} chars).
              </span>
            )}
            {nameCollision && (
              <span className="custom-slash-warn">
                A custom slash named <code>{trimmedName}</code> already exists.
              </span>
            )}
          </label>

          <label>
            <span>description</span>
            <input
              type="text"
              placeholder="What does this slash do?"
              value={draft.description}
              maxLength={DESC_MAX}
              onChange={(e) =>
                setDraft((d) => ({ ...d, description: e.target.value }))
              }
            />
            <span className="custom-slash-counter">
              {draft.description.length}/{DESC_MAX}
            </span>
          </label>

          <label>
            <span>body — one slash per line</span>
            <textarea
              rows={8}
              placeholder={"/workflow morning-standup\n/summary"}
              value={draft.body}
              onChange={(e) =>
                setDraft((d) => ({ ...d, body: e.target.value }))
              }
            />
            <span className="custom-slash-hint muted">
              {lineCount(draft.body)} step
              {lineCount(draft.body) === 1 ? "" : "s"}
              {bodyTooBig && (
                <span className="custom-slash-warn">
                  {" "}— body exceeds {BODY_MAX} char limit.
                </span>
              )}
            </span>
          </label>

          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              Close
            </button>
            <button
              type="button"
              className="btn-primary"
              onClick={() => void handleSave()}
              disabled={!canSave}
            >
              {busy ? "Saving…" : editingName ? "Update" : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// Imperative summoner — detached React root on document.body, torn down on
// close. Matches `openKeyVaultPanel` / `openIDEExportModal`.
let activeRoot: Root | null = null;

export function openCustomSlashBuilder(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "custom-slash-builder";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<CustomSlashBuilder onClose={close} />);
}

export default CustomSlashBuilder;
