/**
 * Skill builder modal — composes a new `~/.cortex/skills/<name>/SKILL.md`
 * file from a small form. Mirrors the layout of the other modals in this
 * codebase (zinc surface + amber accent, `.modal` container).
 *
 * The form captures:
 *   - name (kebab-case, warns on collision with existing skills)
 *   - description (200 char soft cap)
 *   - inputs (name + optional pipe-separated options like `vitest|jest`)
 *   - body (Handlebars-style template, `{{var}}` substitutions)
 *
 * On save we hand the resulting frontmatter+body to a backend
 * `save_skill` command. That command isn't shipped yet in the Rust
 * layer — when it's missing we surface a warning toast-style hint and
 * still close cleanly so the user isn't trapped behind a non-existent
 * endpoint. Once the backend lands, this UI lights up automatically.
 */
import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Skill } from "@/lib/skills";

interface InputDraft {
  name: string;
  /** Raw pipe-separated options text — parsed into an array on save. */
  optionsText: string;
}

interface SkillBuilderModalProps {
  /** Existing skills used to flag name collisions inline. */
  existing: Skill[];
  /** Close the modal — host owns the open/closed state. */
  onClose: () => void;
  /** Fired after a successful save so the host can `reload()` the list. */
  onSaved?: () => void;
}

const DESCRIPTION_MAX = 200;
const KEBAB_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/** Compose the SKILL.md text from the form fields. Each input is rendered
 *  as `- <name>: <options or "string">` in the frontmatter so it round-trips
 *  through the existing loader. */
function renderSkillMd(args: {
  name: string;
  description: string;
  inputs: InputDraft[];
  body: string;
}): string {
  const inputLines = args.inputs
    .filter((i) => i.name.trim().length > 0)
    .map((i) => {
      const opts = i.optionsText
        .split("|")
        .map((s) => s.trim())
        .filter(Boolean);
      const valuePart = opts.length > 0 ? opts.join("|") : "string";
      return `  - ${i.name.trim()}: ${valuePart}`;
    });
  const inputsBlock = inputLines.length > 0 ? `inputs:\n${inputLines.join("\n")}\n` : "";
  // Trailing newline on body keeps editors that auto-trim happy and
  // matches what `loader.rs` writes back when it rehydrates the file.
  const body = args.body.endsWith("\n") ? args.body : `${args.body}\n`;
  return (
    `---\n` +
    `name: ${args.name}\n` +
    `description: ${args.description}\n` +
    inputsBlock +
    `---\n` +
    body
  );
}

export function SkillBuilderModal({
  existing,
  onClose,
  onSaved,
}: SkillBuilderModalProps) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [inputs, setInputs] = useState<InputDraft[]>([
    { name: "", optionsText: "" },
  ]);
  const [body, setBody] = useState("");
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  // Esc closes — matches every other modal in the app.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const trimmedName = name.trim();
  const existingNames = useMemo(
    () => new Set(existing.map((s) => s.name)),
    [existing],
  );
  const nameInvalid = trimmedName.length > 0 && !KEBAB_RE.test(trimmedName);
  const nameCollision = existingNames.has(trimmedName);

  const canSave =
    trimmedName.length > 0 &&
    !nameInvalid &&
    !nameCollision &&
    description.trim().length > 0 &&
    body.trim().length > 0 &&
    !saving;

  const updateInput = (idx: number, patch: Partial<InputDraft>) => {
    setInputs((prev) =>
      prev.map((row, i) => (i === idx ? { ...row, ...patch } : row)),
    );
  };
  const addInput = () => {
    setInputs((prev) => [...prev, { name: "", optionsText: "" }]);
  };
  const removeInput = (idx: number) => {
    setInputs((prev) => prev.filter((_, i) => i !== idx));
  };

  async function handleSave() {
    if (!canSave) return;
    setSaving(true);
    setStatus(null);

    const cleanInputs = inputs
      .filter((i) => i.name.trim().length > 0)
      .map((i) => ({
        name: i.name.trim(),
        options: i.optionsText
          .split("|")
          .map((s) => s.trim())
          .filter(Boolean),
      }));

    const frontmatter = {
      name: trimmedName,
      description: description.trim(),
      inputs: cleanInputs,
    };
    const md = renderSkillMd({
      name: trimmedName,
      description: description.trim(),
      inputs,
      body,
    });

    try {
      // Backend may not implement `save_skill` yet — degrade gracefully on
      // the 404-style failure rather than blocking the user.
      await invoke<void>("save_skill", {
        name: trimmedName,
        body: md,
        frontmatter,
      });
      onSaved?.();
      onClose();
    } catch (err) {
      console.warn("save_skill failed", err);
      setStatus(
        "Couldn't save — the save_skill backend command isn't available yet. Closing without writing.",
      );
      // Close on a short delay so the user reads the message; still call
      // onSaved so the panel refreshes (in case the call did write).
      window.setTimeout(() => {
        onSaved?.();
        onClose();
      }, 1200);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="modal-backdrop" onMouseDown={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}>
      <div
        className="modal skill-builder-modal"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h2>New skill</h2>

        <label>
          <span>name</span>
          <input
            type="text"
            placeholder="kebab-case-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
          {nameInvalid && (
            <span className="skill-builder-warn">
              Use lowercase letters, digits, and single hyphens only.
            </span>
          )}
          {!nameInvalid && nameCollision && (
            <span className="skill-builder-warn">
              A skill named <code>{trimmedName}</code> already exists — pick another name.
            </span>
          )}
        </label>

        <label>
          <span>description</span>
          <input
            type="text"
            placeholder="What does this skill do?"
            value={description}
            maxLength={DESCRIPTION_MAX}
            onChange={(e) => setDescription(e.target.value)}
          />
          <span className="skill-builder-counter">
            {description.length}/{DESCRIPTION_MAX}
          </span>
        </label>

        <div className="skill-builder-inputs">
          <div className="skill-builder-inputs-head">
            <span>inputs</span>
            <button
              type="button"
              className="link-btn"
              onClick={addInput}
            >
              + add input
            </button>
          </div>
          {inputs.map((row, idx) => (
            <div className="skill-builder-input-row" key={idx}>
              <input
                type="text"
                placeholder="varname"
                value={row.name}
                onChange={(e) => updateInput(idx, { name: e.target.value })}
              />
              <input
                type="text"
                placeholder="options: vitest|jest|cargo (optional)"
                value={row.optionsText}
                onChange={(e) =>
                  updateInput(idx, { optionsText: e.target.value })
                }
              />
              <button
                type="button"
                className="link-btn skill-builder-input-remove"
                onClick={() => removeInput(idx)}
                disabled={inputs.length === 1}
                aria-label="remove input"
              >
                ×
              </button>
            </div>
          ))}
        </div>

        <label>
          <span>body</span>
          <textarea
            rows={8}
            placeholder="Markdown template body. Use {{varname}} to reference inputs."
            value={body}
            onChange={(e) => setBody(e.target.value)}
          />
          <span className="skill-builder-hint muted">
            Reference declared inputs with <code>{`{{varname}}`}</code>. Unknown vars error on run.
          </span>
        </label>

        {status && <div className="skill-builder-status muted">{status}</div>}

        <div className="modal-actions">
          <button type="button" onClick={onClose} disabled={saving}>
            Cancel
          </button>
          <button
            type="button"
            className="btn-primary"
            onClick={() => void handleSave()}
            disabled={!canSave}
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
