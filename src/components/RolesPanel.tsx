import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";
import {
  applyRoleToAgent,
  deleteRole,
  listRoles,
  setRole,
  type Role,
} from "@/lib/roles";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";
import type { AgentDescriptor } from "@/lib/cortex-bridge";

interface RolesPanelProps {
  /** Available agents shown in the per-row "Apply to" dropdown. */
  agents: AgentDescriptor[];
  /** Optional fallback agent id when a row's dropdown is left untouched. */
  defaultAgentId?: string | null;
}

/** Mirror of the backend `is_safe_name` check so we fail fast in the UI. */
function isSafeName(name: string): boolean {
  const t = name.trim();
  return t.length > 0 && !t.includes("/") && !t.includes("\\") && !t.includes("..");
}

/** Draft form state — strings throughout so the inputs stay controlled. */
interface ModeDraft {
  name: string;
  description: string;
  model: string;
  tools: string;
  system_prompt: string;
  /** When set, we're editing an existing mode (name is locked). */
  original: string | null;
}

const EMPTY_DRAFT: ModeDraft = {
  name: "",
  description: "",
  model: "",
  tools: "",
  system_prompt: "",
  original: null,
};

function draftFromRole(role: Role): ModeDraft {
  return {
    name: role.name,
    description: role.description ?? "",
    model: role.model ?? "",
    tools: (role.tools ?? []).join(", "),
    system_prompt: role.system_prompt ?? "",
    original: role.name,
  };
}

/** Split a comma/space/newline-separated tool list into a clean array. */
function parseTools(raw: string): string[] {
  return raw
    .split(/[\s,]+/)
    .map((t) => t.trim())
    .filter(Boolean);
}

/**
 * List + author the agent personas ("modes") stored at `~/.cortex/roles/*.yaml`
 * (Cline/Roo custom modes). Each row applies the mode's `system_prompt` to a
 * chosen agent (the chat pipeline picks it up on the next turn) and can be
 * edited or deleted in place; the "+ New role" form creates one from scratch —
 * so a user never has to hand-write YAML, matching Cline/Roo's in-app mode
 * authoring.
 *
 * Kept dependency-free w.r.t. the Zustand store so this can also be summoned
 * from a portal modal later (same pattern as KeyVaultPanel).
 */
export function RolesPanel({ agents, defaultAgentId }: RolesPanelProps) {
  const [roles, setRoles] = useState<Role[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Per-row override: maps roleName → selected agent id. We don't force a
  // pick — the row's "Apply" button falls back to `defaultAgentId` when this
  // map has no entry. Keeping the UX one-click for the common case.
  const [selections, setSelections] = useState<Record<string, string>>({});
  // Active create/edit draft, or null when the form is closed.
  const [draft, setDraft] = useState<ModeDraft | null>(null);
  const [saving, setSaving] = useState(false);

  const refresh = () =>
    listRoles()
      .then((r) => {
        setRoles(r);
        setError(null);
      })
      .catch((e) => setError(humanizeError(e)));

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    listRoles()
      .then((r) => {
        if (!cancelled) {
          setRoles(r);
          setError(null);
        }
      })
      .catch((e) => {
        if (!cancelled) setError(humanizeError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleApply = async (role: Role) => {
    const target = selections[role.name] ?? defaultAgentId ?? agents[0]?.id;
    if (!target) {
      pushToast({
        title: "No agent",
        body: "Connect an agent before applying a role.",
        kind: "warning",
      });
      return;
    }
    try {
      await applyRoleToAgent(role.name, target);
      pushToast({
        title: "Role applied",
        body: `${role.name} → ${target}`,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Apply failed", body: humanizeError(e), kind: "error" });
    }
  };

  const handleSave = async () => {
    if (!draft) return;
    const name = draft.name.trim();
    if (!isSafeName(name)) {
      pushToast({
        title: "Invalid name",
        body: "A role name can't be empty or contain / \\ or ..",
        kind: "warning",
      });
      return;
    }
    // Guard against silently shadowing a different mode when creating new.
    if (!draft.original && roles.some((r) => r.name === name)) {
      pushToast({
        title: "Name taken",
        body: `A role named "${name}" already exists.`,
        kind: "warning",
      });
      return;
    }
    const tools = parseTools(draft.tools);
    const role: Role = {
      name,
      description: draft.description.trim() || undefined,
      model: draft.model.trim() || undefined,
      tools: tools.length ? tools : undefined,
      system_prompt: draft.system_prompt.trim() || undefined,
    };
    setSaving(true);
    try {
      await setRole(role);
      await refresh();
      setDraft(null);
      pushToast({
        title: draft.original ? "Role updated" : "Role created",
        body: name,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (role: Role) => {
    if (!(await confirmDialog({
      title: "Delete role?",
      message: `Delete the "${role.name}" role? This can't be undone.`,
      confirmLabel: "Delete",
      danger: true,
    }))) {
      return;
    }
    try {
      await deleteRole(role.name);
      await refresh();
      if (draft?.original === role.name) setDraft(null);
      pushToast({ title: "Role deleted", body: role.name, kind: "success" });
    } catch (e) {
      pushToast({ title: "Delete failed", body: humanizeError(e), kind: "error" });
    }
  };

  if (loading) {
    return <PanelLoading className="roles-panel" label="Loading roles" />;
  }
  if (error) {
    return (
      <div className="roles-panel role-error">roles unavailable: {error}</div>
    );
  }

  // Shared create/edit form. Rendered inline at the top when `draft` is set.
  const editor = draft && (
    <div className="role-editor">
      <div className="role-editor-head">
        {draft.original ? `Edit "${draft.original}"` : "New role"}
      </div>
      <label className="role-field">
        <span>Name</span>
        <input
          type="text"
          value={draft.name}
          placeholder="e.g. refactorer"
          disabled={!!draft.original}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        />
      </label>
      <label className="role-field">
        <span>Description</span>
        <input
          type="text"
          value={draft.description}
          placeholder="One line — what this role is for"
          onChange={(e) => setDraft({ ...draft, description: e.target.value })}
        />
      </label>
      <label className="role-field">
        <span>Model</span>
        <input
          type="text"
          value={draft.model}
          placeholder="Optional — e.g. claude-opus-4-8"
          onChange={(e) => setDraft({ ...draft, model: e.target.value })}
        />
      </label>
      <label className="role-field">
        <span>Tools</span>
        <input
          type="text"
          value={draft.tools}
          placeholder="Optional — read_file, ripgrep, git_diff"
          onChange={(e) => setDraft({ ...draft, tools: e.target.value })}
        />
      </label>
      <label className="role-field">
        <span>System prompt</span>
        <textarea
          rows={5}
          value={draft.system_prompt}
          placeholder="The persona / instructions applied when this role is active"
          onChange={(e) =>
            setDraft({ ...draft, system_prompt: e.target.value })
          }
        />
      </label>
      <div className="role-editor-actions">
        <button
          type="button"
          className="btn-primary role-save-btn"
          onClick={handleSave}
          disabled={saving}
        >
          {saving ? "Saving…" : draft.original ? "Save changes" : "Create role"}
        </button>
        <button
          type="button"
          className="role-apply-btn"
          onClick={() => setDraft(null)}
          disabled={saving}
        >
          Cancel
        </button>
      </div>
    </div>
  );

  return (
    <div className="roles-panel">
      {!draft && (
        <button
          type="button"
          className="role-new-btn"
          onClick={() => setDraft({ ...EMPTY_DRAFT })}
        >
          + New role
        </button>
      )}
      {editor}
      {roles.length === 0 && !draft && (
        <div className="muted role-empty">
          No roles yet — create one above to define a reusable persona.
        </div>
      )}
      {roles.map((role) => (
        <div key={role.name} className="role-row">
          <div className="role-row-head">
            <strong className="role-name">{role.name}</strong>
            {role.model && (
              <span className="role-model" title="Suggested model">
                {role.model}
              </span>
            )}
          </div>
          {role.description && (
            <div className="muted role-desc">{role.description}</div>
          )}
          <div className="role-row-actions">
            <select
              className="role-agent-select"
              aria-label={`Pick an agent for ${role.name}`}
              value={selections[role.name] ?? defaultAgentId ?? agents[0]?.id ?? ""}
              onChange={(e) =>
                setSelections((s) => ({ ...s, [role.name]: e.target.value }))
              }
            >
              {agents.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.label}
                </option>
              ))}
            </select>
            <button
              type="button"
              className="role-apply-btn"
              onClick={() => handleApply(role)}
              disabled={agents.length === 0}
            >
              Apply
            </button>
            <button
              type="button"
              className="role-icon-btn"
              title={`Edit ${role.name}`}
              aria-label={`Edit ${role.name}`}
              onClick={() => setDraft(draftFromRole(role))}
            >
              Edit
            </button>
            <button
              type="button"
              className="role-icon-btn role-icon-btn--danger"
              title={`Delete ${role.name}`}
              aria-label={`Delete ${role.name}`}
              onClick={() => handleDelete(role)}
            >
              Delete
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
