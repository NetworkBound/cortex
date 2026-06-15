/**
 * Shared validated Obsidian-vault picker — the one "point Cortex at a vault"
 * input. Extracted from SetupPanel section A so the OnboardingWizard reuses
 * the exact same live validation (debounced `validate_obsidian_vault`: does
 * the directory exist? does it contain `.obsidian`?) instead of shipping an
 * unvalidated free-text clone.
 *
 * Controlled: the parent owns the path string; this component owns the
 * debounce + validation round-trip and mirrors every result up through
 * `onValidation` so parents can gate their submit/next buttons on
 * `info.is_valid`.
 */
import { useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { validateObsidianVault, type VaultInfo } from "@/lib/cortex-bridge";
import { pushToast } from "@/lib/toast";
import { humanizeError } from "@/lib/errors";
import "../styles/setup.css";

export function VaultField({
  value,
  onChange,
  onValidation,
  disabled = false,
  label = "Vault folder",
  placeholder = "~/Documents/Cortex Brain",
}: {
  value: string;
  onChange: (path: string) => void;
  /** Fired with every validation result (null while blank or on probe failure). */
  onValidation?: (info: VaultInfo | null) => void;
  disabled?: boolean;
  label?: string;
  placeholder?: string;
}) {
  const [info, setInfo] = useState<VaultInfo | null>(null);
  // Ref so an inline `onValidation` closure doesn't restart the debounce
  // timer on every parent render.
  const onValidationRef = useRef(onValidation);
  onValidationRef.current = onValidation;

  useEffect(() => {
    const p = value.trim();
    const report = (i: VaultInfo | null) => {
      setInfo(i);
      onValidationRef.current?.(i);
    };
    if (!p) {
      report(null);
      return;
    }
    const id = setTimeout(() => {
      validateObsidianVault(p)
        .then(report)
        .catch(() => report(null));
    }, 300);
    return () => clearTimeout(id);
  }, [value]);

  async function browse() {
    try {
      const selected = await openDialog({
        directory: true,
        multiple: false,
        title: "Select Obsidian vault folder",
      });
      if (typeof selected === "string" && selected.length > 0) onChange(selected);
    } catch (e) {
      pushToast({ title: "Couldn't open picker", body: humanizeError(e), kind: "error" });
    }
  }

  return (
    <>
      <div className="setup-field">
        <span className="setup-field-label">{label}</span>
        <div className="setup-input-row">
          <input
            className="setup-input"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={placeholder}
            disabled={disabled}
          />
          <button className="setup-btn" onClick={() => void browse()} disabled={disabled}>
            Browse…
          </button>
        </div>
      </div>
      {info && <VaultStatus info={info} />}
    </>
  );
}

export function VaultStatus({ info }: { info: VaultInfo }) {
  if (!info.is_valid) {
    return <span className="setup-status bad">✗ Directory does not exist</span>;
  }
  if (!info.is_obsidian_vault) {
    return (
      <span className="setup-status warn">
        ⚠ No .obsidian folder found — connecting anyway is allowed
      </span>
    );
  }
  return <span className="setup-status ok">✓ Valid Obsidian vault</span>;
}
