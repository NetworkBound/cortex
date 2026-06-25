/**
 * Recipe gallery modal — Goose-style YAML-recipe browser.
 *
 * Two tabs:
 *  - **Local** — recipes from `~/.cortex/recipes/`. Per-row Run / Edit /
 *    Delete / Share actions. "New" creates a blank recipe from the seed
 *    template.
 *  - **Browse community** — hardcoded seed URLs (we don't host a real
 *    gallery service). Click "Install" on a row, or paste any HTTPS URL
 *    into the "Install from URL" field.
 *
 * Self-mounting portal — same pattern as IDEExportModal so /recipes can
 * summon it without App.tsx wiring.
 */

import { useCallback, useEffect, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  COMMUNITY_SEEDS,
  NEW_RECIPE_TEMPLATE,
  deleteRecipe,
  installRecipeFromUrl,
  listRecipes,
  saveRecipe,
  type Recipe,
} from "@/lib/recipes";
import { pushToast } from "@/lib/toast";

type Tab = "local" | "community";

interface RecipeGalleryProps {
  onClose: () => void;
}

function deriveName(yaml: string, fallback: string): string {
  // Pull `name:` off the first non-comment top-level line. Good enough for
  // the "save under what filename?" flow — the backend re-derives this on
  // its own anyway.
  const match = yaml.match(/^name:\s*(.+?)\s*$/m);
  const name = match?.[1]?.replace(/^["']|["']$/g, "").trim();
  return name || fallback;
}

export function RecipeGallery({ onClose }: RecipeGalleryProps) {
  const [tab, setTab] = useState<Tab>("local");
  const [recipes, setRecipes] = useState<Recipe[]>([]);
  const [editing, setEditing] = useState<{ original: string; yaml: string } | null>(
    null,
  );
  const [installUrl, setInstallUrl] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setRecipes(await listRecipes());
    } catch (err) {
      pushToast({
        title: "Couldn't list recipes",
        body: humanizeError(err),
        kind: "error",
      });
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // ESC closes the modal (or the inline editor first). Matches the
  // convention across the rest of the portal modals.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (editing) setEditing(null);
      else onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editing, onClose]);

  const handleRun = (r: Recipe) => {
    // We don't have a recipe-runtime yet — wire the goal into the chat
    // composer so the user can review and fire it manually. Same UX as
    // /workflow's step-emit fallback.
    try {
      window.dispatchEvent(
        new CustomEvent("cortex:recipe-run", {
          detail: { name: r.name, goal: r.goal, recipe: r },
        }),
      );
      pushToast({
        title: `Running ${r.name}`,
        body: r.goal,
        kind: "info",
      });
      onClose();
    } catch (err) {
      pushToast({ title: "Run failed", body: humanizeError(err), kind: "error" });
    }
  };

  const handleEdit = (r: Recipe) => {
    setEditing({ original: r.name, yaml: r.yaml });
  };

  const handleNew = () => {
    setEditing({ original: "", yaml: NEW_RECIPE_TEMPLATE });
  };

  const handleDelete = async (r: Recipe) => {
    if (!(await confirmDialog({
      title: "Delete recipe?",
      message: `'${r.name}' will be deleted. This cannot be undone.`,
      confirmLabel: "Delete",
      danger: true,
    })))
      return;
    try {
      await deleteRecipe(r.name);
      await refresh();
      pushToast({ title: "Recipe deleted", body: r.name, kind: "success" });
    } catch (err) {
      pushToast({
        title: "Delete failed",
        body: humanizeError(err),
        kind: "error",
      });
    }
  };

  const handleShare = async (r: Recipe) => {
    try {
      await navigator.clipboard.writeText(r.yaml);
      pushToast({
        title: "YAML copied",
        body: `${r.name} → clipboard`,
        kind: "success",
      });
    } catch (err) {
      pushToast({ title: "Copy failed", body: humanizeError(err), kind: "error" });
    }
  };

  const handleSaveEditor = async () => {
    if (!editing) return;
    const name = deriveName(editing.yaml, editing.original || "untitled");
    setBusy(true);
    try {
      await saveRecipe(name, editing.yaml);
      pushToast({ title: "Recipe saved", body: name, kind: "success" });
      setEditing(null);
      await refresh();
    } catch (err) {
      pushToast({ title: "Save failed", body: humanizeError(err), kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const handleInstall = async (url: string) => {
    const trimmed = url.trim();
    if (!trimmed) return;
    setBusy(true);
    try {
      const r = await installRecipeFromUrl(trimmed);
      pushToast({
        title: "Recipe installed",
        body: r.name,
        kind: "success",
      });
      setInstallUrl("");
      setTab("local");
      await refresh();
    } catch (err) {
      pushToast({
        title: "Install failed",
        body: humanizeError(err),
        kind: "error",
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="modal-backdrop recipe-gallery-backdrop"
      onClick={onClose}
      role="presentation"
    >
      <div
        className="modal recipe-gallery-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="Recipe gallery"
      >
        <div className="recipe-gallery-header">
          <h2>Recipes</h2>
          <div className="recipe-gallery-tabs">
            <button
              className={tab === "local" ? "active" : ""}
              onClick={() => setTab("local")}
            >
              Local ({recipes.length})
            </button>
            <button
              className={tab === "community" ? "active" : ""}
              onClick={() => setTab("community")}
            >
              Browse community
            </button>
          </div>
          <button className="link-btn" onClick={onClose}>
            Close
          </button>
        </div>

        {editing ? (
          <div className="recipe-gallery-editor">
            <div className="recipe-gallery-editor-head">
              <span className="muted">
                Editing{" "}
                <code>{editing.original || deriveName(editing.yaml, "new")}</code>
              </span>
              <div>
                <button
                  className="link-btn"
                  onClick={() => setEditing(null)}
                  disabled={busy}
                >
                  Cancel
                </button>
                <button
                  className="link-btn primary"
                  onClick={() => void handleSaveEditor()}
                  disabled={busy}
                >
                  Save
                </button>
              </div>
            </div>
            <textarea
              className="recipe-gallery-textarea"
              value={editing.yaml}
              onChange={(e) =>
                setEditing({ ...editing, yaml: e.target.value })
              }
              spellCheck={false}
              aria-label="Recipe YAML"
            />
          </div>
        ) : tab === "local" ? (
          <div className="recipe-gallery-body">
            <div className="recipe-gallery-toolbar">
              <button className="link-btn" onClick={handleNew}>
                + New recipe
              </button>
            </div>
            {recipes.length === 0 ? (
              <div className="recipe-gallery-empty">
                <div>No recipes yet.</div>
                <div className="muted">
                  Click <strong>+ New recipe</strong> or install one from the
                  community tab.
                </div>
              </div>
            ) : (
              <ul className="recipe-gallery-list">
                {recipes.map((r) => (
                  <li key={r.name} className="recipe-gallery-row">
                    <div className="recipe-gallery-row-main">
                      <div className="recipe-gallery-row-name">{r.name}</div>
                      <div className="recipe-gallery-row-desc muted">
                        {r.description || r.goal}
                      </div>
                      <div className="recipe-gallery-row-meta muted">
                        {r.tools.length > 0 && (
                          <span>tools: {r.tools.join(", ")}</span>
                        )}
                        {r.agents.length > 0 && (
                          <span> · agents: {r.agents.join(", ")}</span>
                        )}
                      </div>
                    </div>
                    <div className="recipe-gallery-row-actions">
                      <button
                        className="link-btn"
                        onClick={() => handleRun(r)}
                        title="Send the recipe's goal to the composer"
                      >
                        Run
                      </button>
                      <button className="link-btn" onClick={() => handleEdit(r)}>
                        Edit
                      </button>
                      <button
                        className="link-btn"
                        onClick={() => void handleShare(r)}
                        title="Copy YAML to clipboard"
                      >
                        Share
                      </button>
                      <button
                        className="link-btn danger"
                        onClick={() => void handleDelete(r)}
                      >
                        Delete
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        ) : (
          <div className="recipe-gallery-body">
            <div className="recipe-gallery-install">
              <label htmlFor="recipe-install-url">Install from URL</label>
              <div className="recipe-gallery-install-row">
                <input
                  id="recipe-install-url"
                  type="url"
                  placeholder="https://example.com/recipe.yaml"
                  value={installUrl}
                  onChange={(e) => setInstallUrl(e.target.value)}
                  disabled={busy}
                />
                <button
                  className="link-btn primary"
                  onClick={() => void handleInstall(installUrl)}
                  disabled={busy || !installUrl.trim()}
                >
                  Install
                </button>
              </div>
              <div className="muted recipe-gallery-install-hint">
                HTTPS only · max 64 KiB · existing recipes with the same name
                are NOT overwritten.
              </div>
            </div>
            <ul className="recipe-gallery-list">
              {COMMUNITY_SEEDS.map((s) => (
                <li key={s.url} className="recipe-gallery-row">
                  <div className="recipe-gallery-row-main">
                    <div className="recipe-gallery-row-name">{s.label}</div>
                    <div className="recipe-gallery-row-desc muted">
                      {s.description}
                    </div>
                    <div className="recipe-gallery-row-meta muted">{s.url}</div>
                  </div>
                  <div className="recipe-gallery-row-actions">
                    <button
                      className="link-btn"
                      onClick={() => void handleInstall(s.url)}
                      disabled={busy}
                    >
                      Install
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}

/** Imperative summoner — creates a detached root, renders the gallery,
 *  tears down on close. Mirrors `openIDEExportModal` so /recipes can
 *  mount this without touching App.tsx. */
let activeRoot: Root | null = null;

export function openRecipeGallery(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "recipe-gallery";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<RecipeGallery onClose={close} />);
}
