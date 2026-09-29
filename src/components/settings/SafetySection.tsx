import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  applyCiSafeProfile,
  ciSafePolicyPreview,
  exportAuditLog,
  getBuiltinCommandRules,
  getCommandPolicy,
  safeModeStatus,
  setCommandPolicy,
  setSafeMode,
  testCommandPolicy,
  type PolicyDecision,
} from "@/lib/safe-mode";
import {
  LoadingHint,
  SettingsSection,
  SettingsToggle,
  UnsavedBadge,
} from "./Section";

/**
 * "Safety" — Safe Mode (issue 004). One switch that composes the existing
 * gates: tier clamp, approval-policy clamp, command allow/denylist, audit of
 * every tool call. Also hosts the command-policy editor (validate-on-save),
 * a "test a command" dry-run box (shows matched rule + source, NEVER
 * executes), and redacted audit-log export. Default off; the backend is the
 * source of truth.
 */
export function SafetySection({ projectRoot }: { projectRoot: string | null }) {
  const [enabled, setEnabled] = useState<boolean | null>(null); // null = loading
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Policy editor state. `policyLoaded` is the on-disk body the editor was
  // hydrated with, so the unsaved indicator can compare against it.
  const [scope, setScope] = useState<"global" | "project">("global");
  const [policyRaw, setPolicyRaw] = useState<string | null>(null); // null = loading
  const [policyLoaded, setPolicyLoaded] = useState<string>("");
  const [policyErr, setPolicyErr] = useState<string | null>(null);
  const [policyMsg, setPolicyMsg] = useState<string | null>(null);
  const [policyBusy, setPolicyBusy] = useState(false);

  // Dry-run box state.
  const [testCmd, setTestCmd] = useState("");
  const [testResult, setTestResult] = useState<PolicyDecision | null>(null);
  const [testErr, setTestErr] = useState<string | null>(null);

  // Audit export state.
  const [exportMsg, setExportMsg] = useState<string | null>(null);
  const [exportErr, setExportErr] = useState<string | null>(null);
  const [exportBusy, setExportBusy] = useState(false);

  // CI-safe preset state (full scope: issue 004).
  const [forceReadOnly, setForceReadOnly] = useState(false);
  const [presetPreview, setPresetPreview] = useState<string | null>(null);
  const [presetConfirming, setPresetConfirming] = useState(false);
  const [presetBusy, setPresetBusy] = useState(false);
  const [presetMsg, setPresetMsg] = useState<string | null>(null);
  const [presetErr, setPresetErr] = useState<string | null>(null);

  // Built-in destructive-command rules viewer (full scope: issue 004).
  const [builtinRules, setBuiltinRules] = useState<string | null>(null); // null = not loaded yet
  const [builtinErr, setBuiltinErr] = useState<string | null>(null);
  const [showBuiltinRules, setShowBuiltinRules] = useState(false);

  const scopeRoot = scope === "project" ? projectRoot : null;

  useEffect(() => {
    let cancelled = false;
    void safeModeStatus()
      .then((s) => {
        if (!cancelled) {
          setEnabled(s.enabled);
          setForceReadOnly(s.force_read_only);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setEnabled(false);
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // (Re)load the policy file whenever the scope changes.
  useEffect(() => {
    let cancelled = false;
    setPolicyRaw(null);
    setPolicyErr(null);
    setPolicyMsg(null);
    setTestResult(null);
    if (scope === "project" && !projectRoot) {
      setPolicyRaw("");
      setPolicyLoaded("");
      return;
    }
    void getCommandPolicy(scopeRoot)
      .then((raw) => {
        if (!cancelled) {
          setPolicyRaw(raw);
          setPolicyLoaded(raw);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setPolicyRaw("");
          setPolicyLoaded("");
          setPolicyErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, projectRoot]);

  async function toggle(next: boolean) {
    setBusy(true);
    setErr(null);
    try {
      const s = await setSafeMode(next);
      setEnabled(s.enabled);
      setForceReadOnly(s.force_read_only);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function startCiSafePreview() {
    setPresetErr(null);
    setPresetMsg(null);
    try {
      setPresetPreview(await ciSafePolicyPreview());
      setPresetConfirming(true);
    } catch (e) {
      setPresetErr(humanizeError(e));
    }
  }

  async function confirmCiSafe() {
    setPresetBusy(true);
    setPresetErr(null);
    try {
      const s = await applyCiSafeProfile();
      setEnabled(s.enabled);
      setForceReadOnly(s.force_read_only);
      setPresetConfirming(false);
      setPresetMsg(
        "CI-safe preset applied: Safe Mode is on, the sandbox tier is clamped to read-only, and the global command policy was replaced.",
      );
      // Refresh the editor if it's currently showing the global scope, so
      // it doesn't silently go stale relative to what's now on disk.
      if (scope === "global") {
        try {
          const raw = await getCommandPolicy(null);
          setPolicyRaw(raw);
          setPolicyLoaded(raw);
        } catch {
          /* editor will just show the previous body; not fatal */
        }
      }
    } catch (e) {
      setPresetErr(humanizeError(e));
    } finally {
      setPresetBusy(false);
    }
  }

  async function toggleBuiltinRules() {
    const next = !showBuiltinRules;
    setShowBuiltinRules(next);
    if (next && builtinRules === null) {
      try {
        setBuiltinRules(await getBuiltinCommandRules());
      } catch (e) {
        setBuiltinErr(humanizeError(e));
      }
    }
  }

  async function savePolicy() {
    if (policyRaw == null) return;
    setPolicyBusy(true);
    setPolicyErr(null);
    setPolicyMsg(null);
    try {
      await setCommandPolicy(scopeRoot, policyRaw);
      setPolicyLoaded(policyRaw);
      setPolicyMsg("Saved.");
    } catch (e) {
      // Validate-on-save: bad TOML is rejected and the file stays untouched.
      setPolicyErr(humanizeError(e));
    } finally {
      setPolicyBusy(false);
    }
  }

  async function runTest() {
    setTestErr(null);
    setTestResult(null);
    try {
      setTestResult(await testCommandPolicy(projectRoot, testCmd));
    } catch (e) {
      setTestErr(humanizeError(e));
    }
  }

  async function doExport(format: "jsonl" | "csv") {
    setExportBusy(true);
    setExportErr(null);
    setExportMsg(null);
    try {
      const r = await exportAuditLog(format);
      setExportMsg(`Wrote ${r.rows} rows to ${r.path}`);
    } catch (e) {
      setExportErr(humanizeError(e));
    } finally {
      setExportBusy(false);
    }
  }

  const policyDirty = policyRaw !== null && policyRaw !== policyLoaded;

  return (
    <SettingsSection
      title="Safety"
      description={
        <>
          Safe Mode composes the existing gates into a one-switch lockdown.
          While it is on: full-access sandboxes behave as{" "}
          <code>workspace-write</code>; a <code>never</code> approval policy is
          pulled back to <code>untrusted</code>; the command policy below is
          enforced (a denied command is blocked before any approval prompt);
          commands the policy does not explicitly allow are never auto-approved;
          and every tool call is written to the audit log. Off by default —
          nothing changes until you enable it.
        </>
      }
    >
      {enabled === null ? (
        <LoadingHint />
      ) : (
        <SettingsToggle
          checked={enabled}
          disabled={busy}
          onChange={(next) => void toggle(next)}
          label="Enable Safe Mode"
          description="Clamps sandbox and approval policy, enforces the command policy below, and audits every tool call. A shield badge appears in the status bar."
        />
      )}
      {err && <div className="settings-err">{err}</div>}

      <h4>Presets</h4>
      <div className="settings-hint">
        <strong className="settings-emph">CI-safe</strong> is the
        maximum-lockdown preset for an unattended/CI run: the sandbox tier is
        clamped to <code>read-only</code> (regardless of any project's own
        tier), the global command policy is replaced with allowlist mode (a
        command not explicitly matched below asks instead of running) plus the
        built-in destructive-command heuristics, and Safe Mode is turned on.
        This <strong>overwrites</strong> your existing global command policy —
        review the preview before confirming.
        {forceReadOnly && enabled && (
          <>
            {" "}
            <span className="settings-pill ok">
              CI-safe is currently applied
            </span>
          </>
        )}
      </div>
      {!presetConfirming ? (
        <div className="settings-row spaced">
          <button type="button" onClick={() => void startCiSafePreview()}>
            Apply CI-safe preset…
          </button>
        </div>
      ) : (
        <div className="settings-stack stretch spaced">
          <div className="settings-hint">
            This will overwrite <code>~/.cortex/command-policy.toml</code> with:
          </div>
          <pre className="settings-pre">{presetPreview}</pre>
          <div className="settings-row spaced">
            <button
              type="button"
              onClick={() => void confirmCiSafe()}
              disabled={presetBusy}
            >
              {presetBusy ? "Applying…" : "Confirm: apply CI-safe preset"}
            </button>
            <button
              type="button"
              onClick={() => setPresetConfirming(false)}
              disabled={presetBusy}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
      {presetMsg && <div className="settings-hint">{presetMsg}</div>}
      {presetErr && <div className="settings-err">{presetErr}</div>}

      <h4>Command policy</h4>
      <div className="settings-hint">
        Ordered rules in TOML; deny &gt; ask &gt; allow. The project file can
        only <em>narrow</em> the global policy (its <code>allow</code> rules are
        ignored). Saved only if it validates.
      </div>
      <div className="settings-row spaced">
        <label className="settings-check">
          <input
            type="radio"
            name="safety-policy-scope"
            checked={scope === "global"}
            onChange={() => setScope("global")}
          />
          <span>
            Global <code>~/.cortex/command-policy.toml</code>
          </span>
        </label>
        <label className="settings-check">
          <input
            type="radio"
            name="safety-policy-scope"
            checked={scope === "project"}
            disabled={!projectRoot}
            onChange={() => setScope("project")}
          />
          <span>
            Project{" "}
            {projectRoot ? (
              <code>.cortex/command-policy.toml</code>
            ) : (
              "(pick a project first)"
            )}
          </span>
        </label>
      </div>
      {policyRaw === null ? (
        <LoadingHint>Loading policy…</LoadingHint>
      ) : (
        <textarea
          className="settings-mono"
          rows={10}
          spellCheck={false}
          aria-label="Command policy (TOML)"
          value={policyRaw}
          onChange={(e) => {
            setPolicyRaw(e.target.value);
            setPolicyMsg(null);
          }}
        />
      )}
      <div className="settings-row spaced">
        <button
          type="button"
          onClick={() => void savePolicy()}
          disabled={
            policyBusy ||
            policyRaw === null ||
            (scope === "project" && !projectRoot)
          }
        >
          {policyBusy ? "Validating…" : "Validate & save"}
        </button>
        <UnsavedBadge show={policyDirty} />
        {policyMsg && <span className="settings-hint">{policyMsg}</span>}
      </div>
      {policyErr && <div className="settings-err">{policyErr}</div>}

      <div className="settings-row spaced">
        <button type="button" onClick={() => void toggleBuiltinRules()}>
          {showBuiltinRules
            ? "Hide built-in destructive-command rules"
            : "View built-in destructive-command rules"}
        </button>
      </div>
      {showBuiltinRules && (
        <>
          <div className="settings-hint">
            These heuristics (rm -rf, dd, mkfs, fork bombs, git push --force,
            curl/wget piped into a shell, chmod -R 777, …) are{" "}
            <strong>always enforced</strong> while Safe Mode's command policy is
            active, layered under your own rules above. You can copy rules from
            here into the editor to customize the wording or add narrower ones
            of your own, but a built-in Deny/Ask can never be loosened back to
            an Allow — deny/ask always outrank allow, regardless of source
            (fail-closed, same as the narrow-only project-file rule above).
          </div>
          {builtinErr ? (
            <div className="settings-err">{builtinErr}</div>
          ) : builtinRules === null ? (
            <LoadingHint />
          ) : (
            <pre className="settings-pre tall">{builtinRules}</pre>
          )}
        </>
      )}

      <h4>Test a command</h4>
      <div className="settings-hint">
        Dry run against the effective global + project policy. Shows the matched
        rule and its source — nothing is executed.
      </div>
      <div className="settings-row spaced">
        <input
          value={testCmd}
          placeholder="e.g. git push --force"
          aria-label="Command to test"
          onChange={(e) => setTestCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && testCmd.trim()) void runTest();
          }}
        />
        <button
          type="button"
          onClick={() => void runTest()}
          disabled={!testCmd.trim()}
        >
          Test
        </button>
      </div>
      {testErr && <div className="settings-err">{testErr}</div>}
      {testResult && (
        <div className="settings-hint">
          Decision:{" "}
          <strong className="settings-emph">{testResult.action}</strong>
          {" — "}
          {testResult.matched ? (
            <>
              rule <code>{testResult.matched}</code> ({testResult.source})
            </>
          ) : (
            <>no rule matched ({testResult.source})</>
          )}
          {testResult.reason && <> — {testResult.reason}</>}
        </div>
      )}

      <h4>Audit log export</h4>
      <div className="settings-hint">
        Exports the tool-call / Safe Mode audit trail to{" "}
        <code>~/.cortex/audit-export-&lt;ts&gt;</code>, with secrets redacted.
      </div>
      <div className="settings-row spaced">
        <button
          type="button"
          onClick={() => void doExport("jsonl")}
          disabled={exportBusy}
        >
          Export JSONL
        </button>
        <button
          type="button"
          onClick={() => void doExport("csv")}
          disabled={exportBusy}
        >
          Export CSV
        </button>
      </div>
      {exportMsg && <div className="settings-hint">{exportMsg}</div>}
      {exportErr && <div className="settings-err">{exportErr}</div>}
    </SettingsSection>
  );
}
