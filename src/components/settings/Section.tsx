import type { ReactNode } from "react";
import type { RuleActivation } from "@/lib/cortex-bridge";

/**
 * Shared building blocks for every settings section so headings, hint copy,
 * toggles and status pills look identical across tabs.
 *
 * `SettingsSection` renders the same DOM the sibling settings components
 * (PushNotifySettings, FailoverSettings, McpServerSettings) emit by hand —
 * `div.settings-section > h3` — so a tab mixing both stays visually uniform.
 */

export function SettingsSection({
  title,
  description,
  children,
}: {
  /** Sentence-case heading. */
  title: string;
  /** Optional one-paragraph explanation under the heading. */
  description?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="settings-section">
      <h3>{title}</h3>
      {description && <div className="settings-hint spaced">{description}</div>}
      {children}
    </div>
  );
}

/**
 * A checkbox row with a title and a mandatory one-line description — every
 * toggle in Settings explains what it does in the same place, at the same
 * size.
 */
export function SettingsToggle({
  checked,
  onChange,
  label,
  description,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: ReactNode;
  description: ReactNode;
  disabled?: boolean;
}) {
  return (
    <label className="settings-check">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>
        {label}
        <small>{description}</small>
      </span>
    </label>
  );
}

/** "Unsaved changes" pill shown next to explicit Save buttons only. */
export function UnsavedBadge({ show }: { show: boolean }) {
  if (!show) return null;
  return (
    <span className="settings-unsaved" role="status">
      Unsaved changes
    </span>
  );
}

/** Loading placeholder line used while a section fetches its backend state. */
export function LoadingHint({
  children = "Loading…",
}: {
  children?: ReactNode;
}) {
  return <div className="settings-hint">{children}</div>;
}

// Status pill for a provider credential / login state. Green when ready,
// amber otherwise — semantic tokens only so it tracks the active theme.
export function StatusPill({
  ok,
  okLabel,
  offLabel,
}: {
  ok: boolean;
  okLabel: string;
  offLabel: string;
}) {
  return (
    <span className={`settings-pill ${ok ? "ok" : "warn"}`}>
      {ok ? okLabel : offLabel}
    </span>
  );
}

// Compact pill that visualises a rule's activation mode. Colours are
// indicative-only and inherit from the theme tokens so they stay readable in
// both light and dark builds.
const ACTIVATION_LABELS: Record<RuleActivation, string> = {
  alwaysApply: "always",
  globs: "globs",
  description: "desc",
  manual: "manual",
};

export function ActivationBadge({
  activation,
}: {
  activation: RuleActivation;
}) {
  const label = ACTIVATION_LABELS[activation];
  return (
    <span title={`activation: ${activation}`} className="settings-badge">
      {label}
    </span>
  );
}
