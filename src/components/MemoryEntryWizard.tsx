import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Guided modal for creating a new auto-memory entry. Mirrors the
 * IDEExportModal portal pattern so the slash command can summon it
 * without App.tsx wiring.
 *
 * Writes a markdown file with the canonical auto-memory frontmatter
 * (`name` / `description` / `metadata.type`) under
 * `~/.claude/projects/<project-key>/memory/<type>_<slug>.md`. The
 * existing `create_memory_entry` Tauri command refuses to overwrite,
 * giving us a built-in collision guard.
 *
 * MEMORY.md is intentionally NOT touched — the user maintains that
 * hub by hand, so we surface a reminder toast after a successful save.
 */

export type MemoryEntryType = "user" | "feedback" | "project" | "reference";

interface TypeOption {
  id: MemoryEntryType;
  label: string;
  description: string;
}

const TYPES: TypeOption[] = [
  {
    id: "user",
    label: "User",
    description: "Personal preferences or facts about the user (tone, defaults, identity).",
  },
  {
    id: "feedback",
    label: "Feedback",
    description: "A lesson from a mistake — why it happened and how to apply the fix next time.",
  },
  {
    id: "project",
    label: "Project",
    description: "Context about a specific project (paths, ports, conventions, gotchas).",
  },
  {
    id: "reference",
    label: "Reference",
    description: "Standing reference material (infra topology, schemas, naming conventions).",
  },
];

interface MemoryEntryWizardProps {
  onClose: () => void;
  initialTitle?: string;
}

/** Kebab-case slug: lowercase, alphanumerics-and-dashes only, trimmed, capped. */
function slugify(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60);
}

/** Encode an absolute project root into its `~/.claude/projects/-…` directory name. */
function projectKey(root: string): string {
  return "-" + root.replace(/^\/+/, "").replace(/[/\\]/g, "-");
}

function buildFrontmatter(slug: string, description: string, type: MemoryEntryType): string {
  // Description is single-line — sanitise newlines so YAML stays valid.
  const desc = description.replace(/\s+/g, " ").trim();
  return ["---", `name: ${slug}`, `description: ${desc}`, "metadata:", `  type: ${type}`, "---", ""].join("\n");
}

/** Body stub for the structured types. Plain types just get the user's body. */
function defaultBody(type: MemoryEntryType): string {
  if (type === "feedback" || type === "project") {
    return "**Why:**\n\n\n**How to apply:**\n\n";
  }
  return "";
}

export function MemoryEntryWizard({ onClose, initialTitle }: MemoryEntryWizardProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [type, setType] = useState<MemoryEntryType>("project");
  const [title, setTitle] = useState(initialTitle ?? "");
  const [description, setDescription] = useState("");
  const [body, setBody] = useState<string>(() => defaultBody("project"));
  const [bodyTouched, setBodyTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const slug = useMemo(() => slugify(title), [title]);

  // Project key powers the target directory. Fall back to the current
  // claude-migration-bundle key so the wizard still works when no
  // Cortex project is active (matches the MEMORY.md the user maintains).
  const projectDirKey = useMemo(() => {
    if (activeProject?.root) return projectKey(activeProject.root);
    return "-home-user-claude-migration-bundle";
  }, [activeProject]);

  const targetPath = useMemo(() => {
    if (!slug) return "";
    // Tauri resolves `~` server-side via the `create_memory_entry` command
    // path; we keep it cosmetic here for display and let the backend
    // canonicalise. We expand to an absolute path for the actual write.
    return `~/.claude/projects/${projectDirKey}/memory/${type}_${slug}.md`;
  }, [projectDirKey, type, slug]);

  // Re-seed the body stub when the type changes — but only if the user
  // hasn't started typing their own body yet. Avoids clobbering work.
  useEffect(() => {
    if (!bodyTouched) setBody(defaultBody(type));
  }, [type, bodyTouched]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onSave = useCallback(async () => {
    setError(null);
    if (!slug) {
      setError("Title is required (becomes the slug).");
      return;
    }
    if (!description.trim()) {
      setError("Description is required — it becomes the frontmatter `description:`.");
      return;
    }
    setBusy(true);
    try {
      // Resolve `~` to the real home dir for the backend write.
      const { homeDir, join } = await import("@tauri-apps/api/path");
      const home = await homeDir();
      const absPath = await join(
        home,
        ".claude",
        "projects",
        projectDirKey,
        "memory",
        `${type}_${slug}.md`,
      );
      const content = buildFrontmatter(slug, description, type) + body.replace(/\s+$/, "") + "\n";
      await invoke<void>("create_memory_entry", { path: absPath, content });
      pushToast({
        title: "Memory entry created",
        body: `Add a one-liner pointer for \`${type}_${slug}\` to MEMORY.md.`,
        kind: "success",
      });
      onClose();
    } catch (e) {
      // create_memory_entry returns a string error on collision — surface verbatim.
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [slug, description, type, body, projectDirKey, onClose]);

  return (
    <div className="memwiz-backdrop" onMouseDown={onClose}>
      <div
        className="memwiz-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="memwiz-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="memwiz-header">
          <h2 id="memwiz-title">New Memory Entry</h2>
          <button className="memwiz-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="memwiz-summary">
          Creates an auto-memory markdown file with the canonical frontmatter.
          You&rsquo;ll still need to add a pointer to <code>MEMORY.md</code> by hand.
        </p>

        <fieldset className="memwiz-types">
          <legend>Type</legend>
          {TYPES.map((t) => (
            <label key={t.id} className="memwiz-type-row">
              <input
                type="radio"
                name="memwiz-type"
                value={t.id}
                checked={type === t.id}
                onChange={() => setType(t.id)}
              />
              <span className="memwiz-type-label">
                <strong>{t.label}</strong>
                <span className="memwiz-type-desc">{t.description}</span>
              </span>
            </label>
          ))}
        </fieldset>

        <label className="memwiz-field">
          <span>Title</span>
          <input
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="e.g. cortex-memory-wizard"
            autoFocus
          />
          {slug && (
            <span className="memwiz-hint">
              Slug: <code>{slug}</code>
            </span>
          )}
        </label>

        <label className="memwiz-field">
          <span>Description</span>
          <input
            type="text"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="One-liner — becomes the frontmatter description."
          />
        </label>

        <label className="memwiz-field">
          <span>Body</span>
          <textarea
            value={body}
            onChange={(e) => {
              setBody(e.target.value);
              setBodyTouched(true);
            }}
            rows={8}
            placeholder={
              type === "feedback" || type === "project"
                ? "Fill out the Why / How to apply stubs."
                : "Free-form markdown body."
            }
          />
        </label>

        {targetPath && (
          <p className="memwiz-target">
            Will write to <code>{targetPath}</code>
          </p>
        )}

        {error && <div className="memwiz-error">{error}</div>}

        <footer className="memwiz-footer">
          <button className="memwiz-secondary" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button
            className="memwiz-primary"
            onClick={onSave}
            disabled={busy || !slug || !description.trim()}
          >
            {busy ? "Saving…" : "Create entry"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner — detached root on document.body, same teardown
 * pattern as IDEExportModal so App.tsx stays untouched.
 */
let activeRoot: Root | null = null;

export function openMemoryEntryWizard(initialTitle?: string): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "memory-wizard";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<MemoryEntryWizard onClose={close} initialTitle={initialTitle} />);
}
