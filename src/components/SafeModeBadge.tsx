import { Shield } from "lucide-react";
import { useSafeMode } from "@/lib/safe-mode";

/**
 * StatusBar shield badge for Safe Mode (issue 004). Visible ONLY while Safe
 * Mode is on — the default (off) state renders nothing, so the status bar is
 * unchanged for anyone who never enables it. Like the sandbox badge, it is a
 * security-critical indicator and therefore stays visible in compact mode.
 *
 * Live via `useSafeMode()`: `config-changed` hot-reload on
 * `~/.cortex/safe-mode.json` plus a slow poll fallback.
 */
export function SafeModeBadge() {
  const enabled = useSafeMode();
  if (!enabled) return null;
  return (
    <span
      className="status-pill safe-mode-badge"
      style={{ color: "#22c55e", borderColor: "#22c55e" }}
      title={
        "Safe Mode is ON — full-access sandboxes are clamped to workspace-write, " +
        "the command policy is enforced, non-allowlisted commands are never " +
        "auto-approved, and every tool call is audited. Toggle in Settings → Safety."
      }
    >
      <Shield size={14} strokeWidth={1.75} aria-hidden="true" /> SAFE
    </span>
  );
}
