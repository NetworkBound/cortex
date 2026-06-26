import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  deleteSpace,
  emptySpace,
  formatGlobLines,
  listSpaces,
  parseGlobLines,
  saveSpace,
  spaceFiles,
  type Space,
} from "@/lib/spaces";
import { confirmDialog } from "@/lib/dialogs";
import { openInEditor } from "@/lib/editor";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Spaces panel — ContextForge #4. Self-mounting portal modal (same pattern as
 * IDEExportModal / KeyVaultPanel / DedupePanel) so the slash command can summon
 * it without App.tsx wiring.
 *
 * Three views, switched on `mode`:
 *   - "list": one row per space with name, description, matched-file count.
 *     Each row has Browse / Edit / Delete buttons; the header has + New space.
 *   - "edit": form for name, description, includes, excludes (textareas, one
 *     glob per line). Validates that name is non-empty; everything else is
 *     optional.
 *   - "browse": shows the list of matched files for a space, clickable to
 *     open in the editor pane (dispatches `cortex:editor-open`).
 *
 * Counts are computed lazily on first render and cached in `countByName` so
 * we don't refire `space_files` every keystroke.
 */

interface SpacesPanelProps {
  /** If set, the panel opens directly into the browse view for this space. */
  initialBrowse?: string;
  onClose: () => void;
}

type Mode =
  | { kind: "list" }
  | { kind: "edit"; draft: Space; isNew: boolean }
  | { kind: "browse"; spaceName: string; files: string[]; loading: boolean };

export function SpacesPanel({ initialBrowse, onClose }: SpacesPanelProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [spaces, setSpaces] = useState<Space[]>([]);
  const [countByName, setCountByName] = useState<Record<string, number>>({});
  const [mode, setMode] = useState<Mode>({ kind: "list" });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // ESC handling mirrors IDEExportModal — close on escape unless we're in a
  // sub-view, in which case escape backs out one level.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (mode.kind !== "list") {
        setMode({ kind: "list" });
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode.kind, onClose]);

  const refresh = useCallback(async () => {
    if (!activeProject) {
      setSpaces([]);
      return;
    }
    try {
      const list = await listSpaces(activeProject.root);
      setSpaces(list);
      // Fire counts in parallel — best-effort, errors get logged not toasted.
      const counts: Record<string, number> = {};
      await Promise.all(
        list.map(async (sp) => {
          try {
            const files = await spaceFiles(activeProject.root, sp.name, 5000);
            counts[sp.name] = files.length;
          } catch {
            counts[sp.name] = 0;
          }
        }),
      );
      setCountByName(counts);
    } catch (e) {
      setError(humanizeError(e));
    }
  }, [activeProject]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Auto-jump into browse view when launched via `/space <name>`.
  useEffect(() => {
    if (!initialBrowse || !activeProject) return;
    void (async () => {
      try {
        const files = await spaceFiles(activeProject.root, initialBrowse, 5000);
        setMode({ kind: "browse", spaceName: initialBrowse, files, loading: false });
      } catch (e) {
        setError(humanizeError(e));
      }
    })();
  }, [initialBrowse, activeProject]);

  const openBrowse = useCallback(
    async (name: string) => {
      if (!activeProject) return;
      setMode({ kind: "browse", spaceName: name, files: [], loading: true });
      try {
        const files = await spaceFiles(activeProject.root, name, 5000);
        setMode({ kind: "browse", spaceName: name, files, loading: false });
      } catch (e) {
        setError(humanizeError(e));
        setMode({ kind: "list" });
      }
    },
    [activeProject],
  );

  const openEdit = useCallback((existing: Space | null) => {
    setError(null);
    setMode({
      kind: "edit",
      draft: existing ? { ...existing } : emptySpace(),
      isNew: existing === null,
    });
  }, []);

  const onDelete = useCallback(
    async (name: string) => {
      if (!activeProject) return;
      if (!(await confirmDialog({
        title: "Delete space?",
        message: `Delete space "${name}"?`,
        confirmLabel: "Delete",
        danger: true,
      }))) return;
      setBusy(true);
      try {
        await deleteSpace(activeProject.root, name);
        pushToast({ title: "Space deleted", body: name, kind: "success" });
        await refresh();
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusy(false);
      }
    },
    [activeProject, refresh],
  );

  const onSave = useCallback(
    async (draft: Space) => {
      if (!activeProject) return;
      const name = draft.name.trim();
      if (!name) {
        setError("Name is required.");
        return;
      }
      setBusy(true);
      try {
        await saveSpace(activeProject.root, { ...draft, name });
        pushToast({ title: "Space saved", body: name, kind: "success" });
        await refresh();
        setMode({ kind: "list" });
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusy(false);
      }
    },
    [activeProject, refresh],
  );

  return (
    <div className="spaces-backdrop" onMouseDown={onClose}>
      <div
        className="spaces-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="spaces-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="spaces-header">
          <h2 id="spaces-title">
            {mode.kind === "browse"
              ? `Files in “${mode.spaceName}”`
              : mode.kind === "edit"
                ? mode.isNew
                  ? "New space"
                  : `Edit “${mode.draft.name}”`
                : "Spaces"}
          </h2>
          <button className="spaces-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        {!activeProject && (
          <p className="spaces-summary">
            <em>No active project — pick one from the sidebar first.</em>
          </p>
        )}

        {activeProject && mode.kind === "list" && (
          <SpacesList
            project={activeProject.name}
            spaces={spaces}
            countByName={countByName}
            busy={busy}
            hasError={!!error}
            onNew={() => openEdit(null)}
            onEdit={(sp) => openEdit(sp)}
            onBrowse={(name) => void openBrowse(name)}
            onDelete={(name) => void onDelete(name)}
          />
        )}

        {activeProject && mode.kind === "edit" && (
          <SpaceForm
            draft={mode.draft}
            isNew={mode.isNew}
            busy={busy}
            onChange={(next) => setMode({ kind: "edit", draft: next, isNew: mode.isNew })}
            onCancel={() => setMode({ kind: "list" })}
            onSave={() => void onSave(mode.draft)}
          />
        )}

        {activeProject && mode.kind === "browse" && (
          <BrowseList
            files={mode.files}
            loading={mode.loading}
            onOpen={(path) => {
              // Resolve relative -> absolute against the active project root
              // so EditorPane can `fs::read_to_string` it directly.
              const abs = `${activeProject.root.replace(/\/$/, "")}/${path}`;
              openInEditor(abs);
            }}
            onBack={() => setMode({ kind: "list" })}
          />
        )}

        {error && <div className="spaces-error">{error}</div>}
      </div>
    </div>
  );
}

interface SpacesListProps {
  project: string;
  spaces: Space[];
  countByName: Record<string, number>;
  busy: boolean;
  hasError: boolean;
  onNew: () => void;
  onEdit: (sp: Space) => void;
  onBrowse: (name: string) => void;
  onDelete: (name: string) => void;
}

function SpacesList({
  project,
  spaces,
  countByName,
  busy,
  hasError,
  onNew,
  onEdit,
  onBrowse,
  onDelete,
}: SpacesListProps) {
  return (
    <>
      <p className="spaces-summary">
        Scoped subsets of <strong>{project}</strong>, defined by glob patterns at{" "}
        <code>.cortex/spaces.yaml</code>.
      </p>
      <div className="spaces-toolbar">
        <button className="spaces-primary" onClick={onNew} disabled={busy}>
          + New space
        </button>
      </div>
      {spaces.length === 0 ? (
        // Suppress the empty-state invite when a load error is already shown.
        hasError ? null : (
          <div className="spaces-empty">
            No spaces yet — click <strong>+ New space</strong> to define one.
          </div>
        )
      ) : (
        <ul className="spaces-list">
          {spaces.map((sp) => (
            <li className="spaces-row" key={sp.name}>
              <div className="spaces-row-main">
                <div className="spaces-row-name">{sp.name}</div>
                {sp.description && (
                  <div className="spaces-row-desc">{sp.description}</div>
                )}
                <div className="spaces-row-meta">
                  {countByName[sp.name] ?? 0} file
                  {(countByName[sp.name] ?? 0) === 1 ? "" : "s"} · {sp.includes.length}{" "}
                  include{sp.includes.length === 1 ? "" : "s"}
                  {sp.excludes.length > 0
                    ? ` · ${sp.excludes.length} exclude${sp.excludes.length === 1 ? "" : "s"}`
                    : ""}
                </div>
              </div>
              <div className="spaces-row-actions">
                <button onClick={() => onBrowse(sp.name)} disabled={busy}>
                  Browse
                </button>
                <button onClick={() => onEdit(sp)} disabled={busy}>
                  Edit
                </button>
                <button
                  className="spaces-danger"
                  onClick={() => onDelete(sp.name)}
                  disabled={busy}
                >
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

interface SpaceFormProps {
  draft: Space;
  isNew: boolean;
  busy: boolean;
  onChange: (next: Space) => void;
  onCancel: () => void;
  onSave: () => void;
}

function SpaceForm({ draft, isNew, busy, onChange, onCancel, onSave }: SpaceFormProps) {
  // Memoize the textarea bodies so re-renders don't blow away the user's
  // in-progress edits (the form is a controlled component anyway, but the
  // join/split round-trip on every keystroke risks cursor jumps without this).
  const includesText = useMemo(() => formatGlobLines(draft.includes), [draft.includes]);
  const excludesText = useMemo(() => formatGlobLines(draft.excludes), [draft.excludes]);

  return (
    <div className="spaces-form">
      <label className="spaces-field">
        <span>Name</span>
        <input
          type="text"
          value={draft.name}
          placeholder="frontend"
          disabled={busy || !isNew}
          onChange={(e) => onChange({ ...draft, name: e.target.value })}
        />
        {!isNew && (
          <small className="spaces-hint">Rename: delete and recreate.</small>
        )}
      </label>

      <label className="spaces-field">
        <span>Description</span>
        <input
          type="text"
          value={draft.description}
          placeholder="React + TS components"
          disabled={busy}
          onChange={(e) => onChange({ ...draft, description: e.target.value })}
        />
      </label>

      <label className="spaces-field">
        <span>Includes (one glob per line)</span>
        <textarea
          rows={4}
          value={includesText}
          placeholder={"src/**/*.tsx\nsrc/**/*.css"}
          disabled={busy}
          onChange={(e) => onChange({ ...draft, includes: parseGlobLines(e.target.value) })}
        />
      </label>

      <label className="spaces-field">
        <span>Excludes (one glob per line)</span>
        <textarea
          rows={3}
          value={excludesText}
          placeholder="src-tauri/**"
          disabled={busy}
          onChange={(e) => onChange({ ...draft, excludes: parseGlobLines(e.target.value) })}
        />
      </label>

      <footer className="spaces-footer">
        <button className="spaces-secondary" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
        <button className="spaces-primary" onClick={onSave} disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </button>
      </footer>
    </div>
  );
}

interface BrowseListProps {
  files: string[];
  loading: boolean;
  onOpen: (path: string) => void;
  onBack: () => void;
}

function BrowseList({ files, loading, onOpen, onBack }: BrowseListProps) {
  return (
    <>
      <p className="spaces-summary">
        {loading
          ? "Resolving globs…"
          : `${files.length} file${files.length === 1 ? "" : "s"} matched.`}
      </p>
      {files.length === 0 && !loading ? (
        <div className="spaces-empty">No files match this space's globs.</div>
      ) : (
        <ul className="spaces-files">
          {files.map((p) => (
            <li key={p}>
              <button className="spaces-file-link" onClick={() => onOpen(p)}>
                <code>{p}</code>
              </button>
            </li>
          ))}
        </ul>
      )}
      <footer className="spaces-footer">
        <button className="spaces-secondary" onClick={onBack}>
          ← Back
        </button>
      </footer>
    </>
  );
}

/**
 * Imperative summoner used by `/spaces` and `/space <name>`. Creates a
 * detached root mounted on `document.body` and tears it down on close —
 * matches the IDEExportModal pattern.
 */
let activeRoot: Root | null = null;

export function openSpacesPanel(initialBrowse?: string): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "spaces";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<SpacesPanel initialBrowse={initialBrowse} onClose={close} />);
}
