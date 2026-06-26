import { useEffect, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  APPROVAL_POLICIES,
  APPROVAL_POLICY_META,
  DEFAULT_APPROVAL_POLICY,
  getApprovalPolicy,
  setApprovalPolicy,
  type ApprovalPolicy,
} from "@/lib/approvals";
import {
  DEFAULT_SANDBOX_TIER,
  SANDBOX_TIERS,
  SANDBOX_TIER_META,
  getSandboxTier,
  setSandboxTier,
  type SandboxTier,
} from "@/lib/sandbox";
import { useCortexStore } from "@/state/store";

/**
 * StatusBar pill that displays — and lets the user pick — the current
 * three-tier sandbox setting for the active project. Hidden when there is
 * no active project (nothing to anchor `.cortex/sandbox.toml` against).
 */
export function SandboxBadge() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [tier, setTier] = useState<SandboxTier>(DEFAULT_SANDBOX_TIER);
  const [policy, setPolicy] = useState<ApprovalPolicy>(DEFAULT_APPROVAL_POLICY);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const popoverRef = useRef<HTMLDivElement | null>(null);

  // Reload tier + approval policy whenever the active project changes.
  useEffect(() => {
    let cancelled = false;
    if (!activeProject) return;
    void getSandboxTier(activeProject.root)
      .then((t) => {
        if (!cancelled) setTier(t);
      })
      .catch(() => {
        // Backend errors fall back to the visual default — the gate still
        // works server-side on the same default.
      });
    void getApprovalPolicy(activeProject.root)
      .then((p) => {
        if (!cancelled) setPolicy(p);
      })
      .catch(() => {
        // Same fallback contract as the tier above.
      });
    return () => {
      cancelled = true;
    };
  }, [activeProject]);

  // Close on outside click / escape.
  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!popoverRef.current) return;
      if (!popoverRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (!activeProject) return null;

  const meta = SANDBOX_TIER_META[tier];

  async function pick(next: SandboxTier) {
    if (!activeProject || next === tier) {
      setOpen(false);
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      await setSandboxTier(activeProject.root, next);
      setTier(next);
      setOpen(false);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function pickPolicy(next: ApprovalPolicy) {
    if (!activeProject || next === policy) return;
    setBusy(true);
    setErr(null);
    try {
      await setApprovalPolicy(activeProject.root, next);
      setPolicy(next);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <span className="sandbox-badge-wrap" ref={popoverRef}>
      <button
        type="button"
        className="status-pill sandbox-badge"
        style={{ color: meta.color, borderColor: meta.color }}
        title={`Sandbox tier: ${meta.label}. Click to change.`}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="dialog"
        aria-expanded={open}
      >
        <span
          className="sandbox-dot"
          style={{ background: meta.color }}
          aria-hidden
        />
        {meta.label}
      </button>
      {open && (
        <div className="sandbox-popover" role="dialog" aria-label="Sandbox tier">
          <div className="sandbox-popover-title">Sandbox tier</div>
          {SANDBOX_TIERS.map((t) => {
            const m = SANDBOX_TIER_META[t];
            return (
              <label
                key={t}
                className={`sandbox-radio${t === tier ? " selected" : ""}`}
                style={t === tier ? { borderColor: m.color } : undefined}
              >
                <input
                  type="radio"
                  name="sandbox-tier"
                  checked={t === tier}
                  disabled={busy}
                  onChange={() => void pick(t)}
                />
                <span className="sandbox-radio-body">
                  <span
                    className="sandbox-radio-label"
                    style={{ color: m.color }}
                  >
                    {m.label}
                  </span>
                  <small className="sandbox-radio-desc">{m.description}</small>
                </span>
              </label>
            );
          })}
          <div className="sandbox-popover-title sandbox-popover-title-sub">
            Approval policy
          </div>
          {APPROVAL_POLICIES.map((p) => {
            const m = APPROVAL_POLICY_META[p];
            return (
              <label
                key={p}
                className={`sandbox-radio${p === policy ? " selected" : ""}`}
              >
                <input
                  type="radio"
                  name="approval-policy"
                  checked={p === policy}
                  disabled={busy}
                  onChange={() => void pickPolicy(p)}
                />
                <span className="sandbox-radio-body">
                  <span className="sandbox-radio-label">{m.label}</span>
                  <small className="sandbox-radio-desc">{m.description}</small>
                </span>
              </label>
            );
          })}
          {err && <div className="sandbox-popover-err">{err}</div>}
        </div>
      )}
    </span>
  );
}
