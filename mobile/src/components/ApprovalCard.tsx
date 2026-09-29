// Approve / Deny card with a risk badge and an optional "remember" toggle.
// Used inline in the Inbox and pinned above the composer in a thread.

import { useState } from "react";
import Icon from "./Icon";
import { Chip } from "./ui";
import type { Approval } from "../lib/types";
import { haptic } from "../lib/native";

const RISK_TONE: Record<string, "ok" | "warn" | "err" | "info"> = {
  read: "ok",
  write: "warn",
  exec: "err",
  network: "info",
};

export function RiskBadge({ risk }: { risk?: string | null }) {
  if (!risk) return null;
  return <Chip tone={RISK_TONE[risk] ?? "warn"}>{risk}</Chip>;
}

export function ApprovalCard({
  approval,
  busy,
  compact,
  focused,
  onDecide,
  onOpen,
}: {
  approval: Approval;
  busy?: boolean;
  /** Inbox row variant: tighter, with an optional "open thread" affordance. */
  compact?: boolean;
  focused?: boolean;
  onDecide: (decision: "approve" | "deny", remember: boolean) => void;
  onOpen?: () => void;
}) {
  const [remember, setRemember] = useState(false);
  const detail = approval.detail ?? approval.preview ?? "";
  return (
    <section
      className={`approval ${compact ? "compact" : ""} ${focused ? "focused" : ""}`}
      aria-busy={busy}
      aria-label={`Approval for ${approval.tool ?? "action"}`}
    >
      <div className="approval-head">
        <Icon name="warning" size={16} className="approval-ico" />
        <span className="approval-tool">
          {approval.tool || "Approval needed"}
        </span>
        <RiskBadge risk={approval.risk} />
        <span className="spacer" />
        {onOpen && (
          <button className="linkbtn" onClick={onOpen}>
            Open
          </button>
        )}
      </div>
      {detail && <pre className="approval-detail">{detail}</pre>}
      <div className="approval-actions">
        <label className="remember">
          <input
            type="checkbox"
            checked={remember}
            onChange={(e) => setRemember(e.target.checked)}
          />
          Remember
        </label>
        <button
          className="btn deny"
          disabled={busy}
          onClick={() => {
            haptic("medium");
            onDecide("deny", remember);
          }}
        >
          Deny
        </button>
        <button
          className="btn approve"
          disabled={busy}
          onClick={() => {
            haptic("success");
            onDecide("approve", remember);
          }}
        >
          {busy ? "…" : "Approve"}
        </button>
      </div>
    </section>
  );
}
