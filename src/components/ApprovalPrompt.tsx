import { useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { approveRun } from "@/lib/cortex-bridge";
import {
  addAutoApprove,
  guessAutoApprovePattern,
} from "@/lib/approvals";
import type { PendingApproval } from "@/state/store";
import { playSound } from "@/lib/sounds";
import { pushToast } from "@/lib/toast";
import { HunkReview, type HunkSelection } from "./HunkReview";

interface Props {
  approval: PendingApproval;
  onResolved: (choice: string) => void;
}

const CHOICE_LABEL: Record<string, string> = {
  once: "Approve once",
  session: "Approve for session",
  always: "Always approve",
  deny: "Deny",
};

const CHOICE_DESC: Record<string, string> = {
  once: "Run this single tool call. Ask me again next time.",
  session: "Auto-approve this tool for the rest of this chat session.",
  always: "Auto-approve this tool permanently (until I rotate).",
  deny: "Reject this call. The agent will see a denial.",
};

/** Best-effort cast of `approval.request` to a JSON object for field lookups. */
function asObject(v: unknown): Record<string, unknown> | null {
  if (v && typeof v === "object" && !Array.isArray(v)) {
    return v as Record<string, unknown>;
  }
  return null;
}

const DIFF_KEYS = ["diff", "patch", "unified_diff"] as const;

/**
 * Returns the diff text if this approval looks edit-shaped, plus the request
 * key it came from (so a line-level subset can be threaded back through the
 * same field via `edited_payload`). `key` is null when the diff was only
 * recovered from the preview — in that case the gateway has no field to override,
 * so line-level apply degrades to whole-hunk selection.
 */
function extractDiff(approval: PendingApproval): {
  text: string;
  key: (typeof DIFF_KEYS)[number] | null;
} | null {
  const obj = asObject(approval.request);
  if (obj) {
    for (const key of DIFF_KEYS) {
      const v = obj[key];
      if (typeof v === "string" && v.includes("@@")) return { text: v, key };
    }
  }
  // Fall back to the preview — the gateway's diff-tool adapter mirrors the patch
  // into the preview slot, so we still surface the hunk UI without `request`.
  if (approval.preview && approval.preview.includes("@@")) {
    return { text: approval.preview, key: null };
  }
  return null;
}

/** Returns the command string if this approval looks like a shell call. */
function extractCommand(approval: PendingApproval): string | null {
  const obj = asObject(approval.request);
  if (obj) {
    for (const key of ["command", "cmd", "shell", "bash"]) {
      const v = obj[key];
      if (typeof v === "string") return v;
    }
  }
  // Heuristic preview fallback for `Bash`/`shell_exec` tools that preview the
  // command line as their first line.
  const tool = (approval.tool ?? "").toLowerCase();
  const looksShell =
    tool.includes("bash") || tool.includes("shell") || tool === "exec";
  if (looksShell && approval.preview) {
    const firstLine = approval.preview.split(/\r?\n/)[0]?.trim();
    if (firstLine) return firstLine;
  }
  return null;
}

/**
 * Heuristic risk classification for an approval. "Danger" means the call grants
 * broad, hard-to-undo, or permanent access — shell execution, filesystem writes,
 * or any global-scope grant ("always allow"). These get a red treatment and the
 * prompt defaults focus to Deny so a stray Enter can't wave them through.
 *
 * The `choice` argument lets us flag the global-scope "always" button itself as
 * dangerous regardless of the tool, since a persistent grant outlives the run.
 */
const DANGER_TOOL_RE =
  /(bash|shell|exec|terminal|command|write|edit|patch|delete|remove|rm\b|create_file|fs_|file_)/i;

function isDangerTool(approval: PendingApproval): boolean {
  const tool = (approval.tool ?? "").toLowerCase();
  if (DANGER_TOOL_RE.test(tool)) return true;
  // A shell-shaped or diff-shaped payload is destructive even if the tool name
  // didn't match (e.g. a generic `run`/`apply` tool).
  if (extractCommand(approval) != null) return true;
  if (extractDiff(approval) != null) return true;
  return false;
}

function isDangerChoice(choice: string): boolean {
  // "always" persists a global-scope grant beyond this session.
  return choice === "always";
}

export function ApprovalPrompt({ approval, onResolved }: Props) {
  const [submitting, setSubmitting] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [savingAllow, setSavingAllow] = useState(false);
  const [savedAllow, setSavedAllow] = useState(false);

  const diffInfo = useMemo(() => extractDiff(approval), [approval]);
  const diffText = diffInfo?.text ?? null;
  const initialCommand = useMemo(() => extractCommand(approval), [approval]);

  // Destructive / broad-access calls get a red treatment and steal initial
  // focus onto Deny so a reflexive Enter can't approve a shell/file grant.
  const danger = useMemo(() => isDangerTool(approval), [approval]);
  const denyRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (danger) denyRef.current?.focus();
    // Only autofocus once per approval — re-running on every render would
    // yank focus away while the user reads the diff.
  }, [danger, approval.id]);

  // Hunk-by-hunk review is opt-in via the toggle when a diff is detected.
  const [reviewHunks, setReviewHunks] = useState(false);
  // The full line-level selection from HunkReview (null until first toggle).
  const [hunkSelection, setHunkSelection] = useState<HunkSelection | null>(
    null,
  );

  // Editable command — only used when this is a shell-shaped approval.
  const [editedCommand, setEditedCommand] = useState<string>(
    initialCommand ?? "",
  );
  const commandChanged =
    initialCommand != null && editedCommand !== initialCommand;

  async function pick(choice: string) {
    setSubmitting(choice);
    setErr(null);
    if (choice !== "deny") playSound("approve");
    try {
      // Build the optional overrides for approve_run:
      //  - editedPayload: only when the user actually changed the shell input
      //  - acceptedHunks: only when hunk review is active AND the user has
      //    deselected at least one hunk (full selection ≈ legacy behavior)
      const opts: Parameters<typeof approveRun>[2] = {};
      if (choice !== "deny" && commandChanged && initialCommand != null) {
        const obj = asObject(approval.request) ?? {};
        // Replace whichever field the original used; default to `command`.
        const key =
          (["command", "cmd", "shell", "bash"] as const).find(
            (k) => typeof obj[k] === "string",
          ) ?? "command";
        opts.editedPayload = { ...obj, [key]: editedCommand };
      }
      if (
        choice !== "deny" &&
        reviewHunks &&
        hunkSelection != null &&
        diffText != null
      ) {
        if (hunkSelection.partial && diffInfo?.key != null) {
          // The user dove below the hunk level. Rebuild a patch containing
          // exactly the chosen lines and thread it back through the same
          // diff field so the gateway applies only that subset. (Needs a real
          // request field to override — preview-only diffs can't.)
          const obj = asObject(approval.request) ?? {};
          opts.editedPayload = {
            ...obj,
            [diffInfo.key]: hunkSelection.filteredDiff,
          };
        } else {
          // Whole-hunk selection (the default) — forward the chosen indices.
          opts.acceptedHunks = hunkSelection.acceptedHunks;
        }
      }
      await approveRun(approval.runId, choice, opts);
      // Only confirm + clear AFTER the backend accepts the decision. On error
      // we deliberately leave the prompt mounted (no optimistic clear) so the
      // user can retry against the still-paused run.
      const toolName = approval.tool ?? "tool call";
      pushToast(
        choice === "deny"
          ? {
              title: "Denied",
              body: `${toolName} was rejected.`,
              kind: "info",
            }
          : {
              title: "Approved",
              body: `${toolName} — ${CHOICE_LABEL[choice] ?? choice}.`,
              kind: "success",
            },
      );
      onResolved(choice);
    } catch (e) {
      // Keep the prompt mounted and surface the failure clearly — the run is
      // still paused, so the user must be able to see the error and retry.
      const msg = humanizeError(e);
      setErr(msg);
      setSubmitting(null);
      playSound("error");
      pushToast({
        title: "Approval failed",
        body: msg,
        kind: "error",
      });
    }
  }

  async function saveAlwaysAllow() {
    setSavingAllow(true);
    setErr(null);
    try {
      const obj = asObject(approval.request);
      const pattern = guessAutoApprovePattern(obj ?? approval.preview ?? "");
      await addAutoApprove({
        tool: approval.tool ?? "",
        pattern,
      });
      setSavedAllow(true);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setSavingAllow(false);
    }
  }

  // When the user deselects EVERY changed line under hunk-review, the only
  // sane submit is Deny — disable the Approve buttons in that case.
  const allHunksRejected =
    reviewHunks &&
    hunkSelection != null &&
    hunkSelection.acceptedLineCount === 0;

  return (
    <div className={`approval-prompt${danger ? " approval-danger" : ""}`}>
      <div className="approval-head">
        <span className="approval-badge">
          {danger ? "destructive — approval required" : "approval required"}
        </span>
        <strong>{approval.tool ?? "tool call"}</strong>
        <div className="approval-head-spacer" />
        <button
          type="button"
          className="link-btn approval-always-allow-link"
          onClick={() => void saveAlwaysAllow()}
          disabled={savingAllow || savedAllow}
          title="Add this tool+pattern to ~/.cortex/auto-approve.json — a permanent, global grant"
        >
          {savedAllow ? "Always allowed" : savingAllow ? "Saving…" : "Always allow this"}
        </button>
      </div>

      {initialCommand != null ? (
        <div className="approval-cmd-edit">
          <label className="approval-cmd-label" htmlFor={`cmd-${approval.id}`}>
            Command (editable)
          </label>
          <input
            id={`cmd-${approval.id}`}
            className="approval-cmd-input"
            value={editedCommand}
            onChange={(e) => setEditedCommand(e.target.value)}
            spellCheck={false}
            autoComplete="off"
            disabled={submitting !== null}
          />
          {commandChanged && (
            <span className="approval-cmd-note">
              edited — the gateway will run the new command
            </span>
          )}
        </div>
      ) : (
        approval.preview && (
          <pre className="approval-preview">{approval.preview}</pre>
        )
      )}

      {diffText && (
        <div className="approval-hunk-toggle">
          <label>
            <input
              type="checkbox"
              checked={reviewHunks}
              onChange={(e) => {
                setReviewHunks(e.target.checked);
                if (!e.target.checked) setHunkSelection(null);
              }}
              disabled={submitting !== null}
            />
            <span>Review hunks individually</span>
          </label>
        </div>
      )}

      {reviewHunks && diffText && (
        <HunkReview diff={diffText} onSelectionChange={setHunkSelection} />
      )}

      <div className="approval-actions">
        {approval.choices.map((c) => {
          // Flag any approve button as dangerous when the call itself is
          // destructive, and always flag the global-scope "always" grant.
          const dangerBtn = c !== "deny" && (danger || isDangerChoice(c));
          return (
            <button
              key={c}
              ref={c === "deny" ? denyRef : undefined}
              className={`approval-btn approval-${c}${dangerBtn ? " approval-btn-danger" : ""}`}
              onClick={() => void pick(c)}
              disabled={
                submitting !== null || (c !== "deny" && allHunksRejected)
              }
              title={
                c !== "deny" && allHunksRejected
                  ? "All hunks rejected — choose Deny instead"
                  : isDangerChoice(c)
                    ? `${CHOICE_DESC[c] ?? c} This is a permanent, global grant.`
                    : CHOICE_DESC[c] ?? c
              }
            >
              {submitting === c ? "…" : (CHOICE_LABEL[c] ?? c)}
            </button>
          );
        })}
      </div>
      {err && <div className="approval-error">{err}</div>}
    </div>
  );
}
