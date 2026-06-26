import { useState } from "react";
import { humanizeError } from "@/lib/errors";
import { approvePlan, type Plan } from "@/lib/plan";

interface PlanCardProps {
  /** Session id the approval event is emitted under. */
  sessionId: string;
  /** Plan blob extracted from a `tool: "plan"` event or assistant payload. */
  plan: Plan;
  /**
   * Called when the user submits a "Modify" note. Owners typically funnel
   * this back into the agent as a new user turn that re-runs plan-iteration.
   * Omit to hide the Modify button entirely.
   */
  onModify?: (feedback: string) => void;
  /** Disable both CTAs once the plan has been acted on. */
  disabled?: boolean;
}

/**
 * Renders an agent's structured plan as an inline card with numbered steps
 * and approve/modify CTAs. Replaces plain markdown rendering when the agent
 * emits a `tool: "plan"` message (Terax #13).
 *
 * Approval delegates to `approve_plan` (Tauri command) which emits a
 * `plan_approved` event the orchestrator picks up. Modify expands an inline
 * textarea — submitting it calls `onModify` so the caller can re-enter
 * plan-iteration mode (typically by chat_send'ing the feedback as the next
 * user message).
 */
export function PlanCard({ sessionId, plan, onModify, disabled }: PlanCardProps) {
  const [busy, setBusy] = useState(false);
  const [approved, setApproved] = useState(false);
  const [editing, setEditing] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [error, setError] = useState<string | null>(null);

  const handleApprove = async () => {
    if (busy || approved || disabled) return;
    setBusy(true);
    setError(null);
    try {
      await approvePlan(sessionId, plan.id);
      setApproved(true);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const handleSubmitModify = () => {
    const trimmed = feedback.trim();
    if (!trimmed || !onModify) return;
    onModify(trimmed);
    setFeedback("");
    setEditing(false);
  };

  const isDisabled = disabled || approved;

  return (
    <div className="plan-card" data-approved={approved ? "true" : "false"}>
      <div className="plan-card-head">
        <span className="plan-card-badge">plan</span>
        <strong className="plan-card-title">{plan.title}</strong>
      </div>
      {plan.summary && (
        <div className="plan-card-summary">{plan.summary}</div>
      )}
      <ol className="plan-card-steps">
        {plan.steps.map((step, i) => (
          <li key={i} className="plan-card-step">
            <div className="plan-card-step-title">{step.title}</div>
            {step.detail && (
              <div className="plan-card-step-detail">{step.detail}</div>
            )}
            {step.estimated_time && (
              <div className="plan-card-step-time">
                est. {step.estimated_time}
              </div>
            )}
          </li>
        ))}
      </ol>
      {(plan.estimated_time || plan.estimated_cost) && (
        <div className="plan-card-meta">
          {plan.estimated_time && (
            <span>est. time: {plan.estimated_time}</span>
          )}
          {plan.estimated_cost && (
            <span>est. cost: {plan.estimated_cost}</span>
          )}
        </div>
      )}
      {error && <div className="plan-card-error">{error}</div>}
      <div className="plan-card-actions">
        <button
          type="button"
          className="plan-card-approve"
          onClick={handleApprove}
          disabled={isDisabled || busy}
        >
          {approved ? "approved" : busy ? "approving…" : "Approve plan"}
        </button>
        {onModify && (
          <button
            type="button"
            className="plan-card-modify"
            onClick={() => setEditing((v) => !v)}
            disabled={isDisabled}
          >
            {editing ? "cancel" : "Modify"}
          </button>
        )}
      </div>
      {editing && onModify && (
        <div className="plan-card-modify-box">
          <textarea
            className="plan-card-modify-input"
            value={feedback}
            onChange={(e) => setFeedback(e.target.value)}
            placeholder="What should change about this plan?"
            rows={3}
            autoFocus
          />
          <div className="plan-card-modify-actions">
            <button
              type="button"
              onClick={handleSubmitModify}
              disabled={!feedback.trim()}
            >
              Send feedback
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
