import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { getAgentInstructions, setAgentInstructions } from "@/lib/profiles";

interface AgentInstructionsEditorProps {
  /** Agent id the instructions are scoped to (e.g. "gateway-remote"). */
  agentId: string;
  /** Pretty label shown in the modal header. Falls back to `agentId`. */
  agentLabel?: string;
  /** Close the modal — host owns the open/closed state. */
  onClose: () => void;
  /** Optional notification after a successful save. */
  onSaved?: (text: string) => void;
}

/**
 * Modal editor for per-agent custom instructions (Terax #8). Backed by
 * `~/.cortex/agent-instructions.json` via the `get/set_agent_instructions`
 * Tauri commands. The textarea preloads with whatever's on disk; saving an
 * empty value clears the entry.
 *
 * Layout intentionally mirrors `SettingsModal` and `ApprovalPrompt` (zinc
 * surface + amber accent) so it slots in without bespoke styling. The char
 * counter exposes itself via the `--cortex-counter` CSS custom property so
 * hosts can theme it without re-writing the component.
 */
export function AgentInstructionsEditor({
  agentId,
  agentLabel,
  onClose,
  onSaved,
}: AgentInstructionsEditorProps) {
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getAgentInstructions(agentId)
      .then((v) => {
        if (!cancelled) setText(v ?? "");
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
  }, [agentId]);

  const handleSave = async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      const stored = await setAgentInstructions(agentId, text);
      onSaved?.(stored);
      onClose();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setSaving(false);
    }
  };

  const handleClear = () => {
    setText("");
  };

  const charCount = text.length;

  return (
    <div
      className="agent-instructions-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="agent-instructions-modal"
        role="dialog"
        aria-label="Agent instructions"
      >
        <div className="agent-instructions-head">
          <strong>Instructions for {agentLabel ?? agentId}</strong>
          <button
            type="button"
            className="agent-instructions-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>
        <div className="agent-instructions-body">
          {loading ? (
            <div className="muted">loading…</div>
          ) : (
            <>
              <textarea
                className="agent-instructions-textarea"
                value={text}
                onChange={(e) => setText(e.target.value)}
                rows={10}
                placeholder="Prepended to this agent's system prompt on every run. Leave blank to clear."
                autoFocus
              />
              <div
                className="agent-instructions-meta"
                style={{ ["--cortex-counter" as never]: `"${charCount} chars"` }}
              >
                <span className="agent-instructions-counter">
                  {charCount} chars
                </span>
                <button
                  type="button"
                  className="agent-instructions-clear"
                  onClick={handleClear}
                  disabled={text.length === 0}
                >
                  Clear
                </button>
              </div>
            </>
          )}
          {error && <div className="agent-instructions-error">{error}</div>}
        </div>
        <div className="agent-instructions-actions">
          <button type="button" onClick={onClose} disabled={saving}>
            Cancel
          </button>
          <button
            type="button"
            className="agent-instructions-save"
            onClick={handleSave}
            disabled={saving || loading}
          >
            {saving ? "saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
