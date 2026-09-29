import { useEffect, useRef, useState } from "react";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { CheckCircle2, CircleDashed, FolderPlus } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import { addProjectViaDialog } from "@/lib/add-project";
import { setObsidianVault } from "@/lib/brain";
import {
  getGatewayConfig,
  getProviderConfig,
  listLocalCliProviders,
  setGatewayApiKey,
  setProviderKey,
  updateGatewayConfig,
  type LocalCliProvider,
  type ProviderConfig,
  type VaultInfo,
} from "@/lib/cortex-bridge";
import { notifyGatewayConfigChanged } from "@/lib/gateway";
import { listModels } from "@/lib/models";
import { listProjects } from "@/lib/projects";
import { applyTheme, loadTheme, THEMES, type ThemeId } from "@/lib/themes";
import { useCortexStore, type ActivityTab } from "@/state/store";
import { VaultField } from "./VaultField";
import "../styles/onboarding-wizard.css";

const ONBOARDED_KEY = "cortex.onboarded";

/** Steps 0–3 are the setup pages; step 4 is the state-driven "next steps"
 *  card shown after Save & launch (what's still missing → where to fix it). */
type StepIndex = 0 | 1 | 2 | 3 | 4;

/** How this install talks to models: through the agent CLIs already on this
 *  machine (Claude Code, Codex, Gemini, …), through a Cortex Gateway, or
 *  straight to providers with the user's own API keys (standalone builds). */
type ConnectMode = "cli" | "gateway" | "standalone";

/** Step 4's title depends on the mode picked on step 1. */
function stepTitle(step: StepIndex, mode: ConnectMode): string {
  switch (step) {
    case 0:
      return "Welcome to Cortex";
    case 1:
      return "Pick a project";
    case 2:
      return "Choose your Obsidian vault";
    case 3:
      return mode === "gateway"
        ? "Connect to your Cortex Gateway"
        : mode === "standalone"
          ? "Add provider API keys"
          : "Local agents ready";
    default:
      return "Setup complete";
  }
}

/** Real app state probed right after setup saves, driving the final card. */
interface NextSteps {
  /** Count of `available` models from the adapter registry. */
  models: number;
  /** Installed local agent CLIs (labels), counted as ready agents too. */
  clis: string[];
  /** The vault path that actually persisted (null = none configured). */
  vault: string | null;
  /** Count of discovered code projects. */
  projects: number;
}

/** Cap a probe: `Promise.allSettled` guards against a rejection but NOT
 *  against a hang, so an unreachable gateway on a fresh install would
 *  otherwise leave "Saving…" stuck forever. */
function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) =>
      setTimeout(() => reject(new Error("probe timed out")), ms),
    ),
  ]);
}

export function OnboardingWizard() {
  const onboardingComplete = useCortexStore((s) => s.onboardingComplete);
  const setOnboardingComplete = useCortexStore((s) => s.setOnboardingComplete);
  const setHasApiKey = useCortexStore((s) => s.setHasApiKey);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const activeProject = useCortexStore((s) => s.activeProject);
  const projectCount = useCortexStore(
    (s) => s.projects.filter((p) => p.kind === "code").length,
  );

  const [step, setStep] = useState<StepIndex>(0);
  const [mode, setMode] = useState<ConnectMode>("gateway");
  // Once the user picks a mode by hand the CLI probe must not override it.
  const modeTouched = useRef(false);
  const [clis, setClis] = useState<LocalCliProvider[] | null>(null);
  const [vaultPath, setVaultPath] = useState("");
  const [vaultInfo, setVaultInfo] = useState<VaultInfo | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [anthropicKey, setAnthropicKey] = useState("");
  const [openaiKey, setOpenaiKey] = useState("");
  const [providerCfg, setProviderCfg] = useState<ProviderConfig | null>(null);
  const [theme, setTheme] = useState<ThemeId>(loadTheme());
  const [saving, setSaving] = useState(false);
  const [nextSteps, setNextSteps] = useState<NextSteps | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // "Pick a project" step: picker in flight + inline outcome.
  const [picking, setPicking] = useState(false);
  const [pickError, setPickError] = useState<string | null>(null);

  // Prefill only the vault path from existing config (it's the user's own
  // filesystem, never a network default). Gateway URL/model deliberately stay
  // EMPTY — blank fields mean "keep whatever is configured", and we never
  // surface a baked-in LAN address to a first-run user.
  useEffect(() => {
    if (onboardingComplete) return;
    getGatewayConfig()
      .then((cfg) => {
        if (!vaultPath && cfg.obsidian_vault) setVaultPath(cfg.obsidian_vault);
      })
      .catch(() => {
        // Non-fatal — the field stays blank; the placeholder still guides.
      });
    // Default the mode from the build: a standalone build running in cloud
    // mode almost certainly wants direct provider keys, everything else
    // (including the homelab build) starts on the gateway flow — unless the
    // CLI probe below finds an installed agent, which wins.
    getProviderConfig()
      .then((cfg) => {
        setProviderCfg(cfg);
        if (
          !modeTouched.current &&
          cfg.standalone_build &&
          cfg.runtime_mode === "cloud"
        )
          setMode("standalone");
      })
      .catch(() => {
        // Non-fatal — mode stays "gateway" and the build note is omitted.
      });
    // Which agent CLIs are already on this machine (filesystem probes only,
    // no network). An installed one makes "local CLIs" the default mode.
    withTimeout(listLocalCliProviders(), 4000)
      .then((list) => {
        setClis(list);
        if (!modeTouched.current && list.some((c) => c.installed))
          setMode("cli");
      })
      .catch(() => setClis([]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onboardingComplete]);

  if (onboardingComplete) return null;

  const installedClis = (clis ?? []).filter((c) => c.installed);

  function finish() {
    try {
      localStorage.setItem(ONBOARDED_KEY, "true");
    } catch {
      // private mode — best-effort; the in-memory flag still gates re-show.
    }
    setOnboardingComplete(true);
  }

  function skipAll() {
    finish();
  }

  /** Close the wizard and land on the tab that fixes the gap. */
  function finishInto(tab: ActivityTab) {
    setActivityTab(tab);
    finish();
  }

  function pickMode(m: ConnectMode) {
    modeTouched.current = true;
    setMode(m);
  }

  /** Native folder picker → register → activate. Stays on this card (no
   *  Projects-tab reveal) and renders the outcome inline. */
  async function pickProject() {
    if (picking) return;
    setPicking(true);
    setPickError(null);
    try {
      const r = await addProjectViaDialog({
        revealProjects: false,
        toastErrors: false,
        title: "Pick a project folder (a git repository)",
      });
      if (r.status === "error") setPickError(r.message);
    } finally {
      setPicking(false);
    }
  }

  function openInstallUrl(url: string) {
    if (!/^https?:\/\//i.test(url)) return;
    shellOpen(url).catch(() => {
      /* no browser handler — the URL is still visible as the hint */
    });
  }

  /** Probe what's actually configured now that setup saved, then show the
   *  final card. Every probe is best-effort — a failed OR slow one reads as
   *  "missing", which only ever points the user at a tab, never blocks them. */
  async function showNextSteps() {
    const [modelsR, cfgR, projectsR, clisR] = await Promise.allSettled([
      withTimeout(listModels(), 4000),
      withTimeout(getGatewayConfig(), 4000),
      withTimeout(listProjects(), 4000),
      withTimeout(listLocalCliProviders(), 4000),
    ]);
    setNextSteps({
      models:
        modelsR.status === "fulfilled"
          ? modelsR.value.filter((m) => m.available).length
          : 0,
      clis:
        clisR.status === "fulfilled"
          ? clisR.value.filter((c) => c.installed).map((c) => c.label)
          : installedClis.map((c) => c.label),
      vault: cfgR.status === "fulfilled" ? cfgR.value.obsidian_vault : null,
      projects:
        projectsR.status === "fulfilled"
          ? projectsR.value.filter((p) => p.kind === "code").length
          : 0,
    });
    setStep(4);
  }

  async function saveAndLaunch() {
    setSaving(true);
    setErr(null);
    try {
      const trimmedVault = vaultPath.trim();
      if (trimmedVault.length > 0) {
        await setObsidianVault(trimmedVault);
      }
      if (mode === "gateway") {
        const updates: {
          gateway_base_url?: string;
          gateway_model?: string;
        } = {};
        const trimmedUrl = baseUrl.trim();
        const trimmedModel = model.trim();
        if (trimmedUrl.length > 0) updates.gateway_base_url = trimmedUrl;
        if (trimmedModel.length > 0) updates.gateway_model = trimmedModel;
        if (Object.keys(updates).length > 0) {
          await updateGatewayConfig(updates);
        }
        const trimmedKey = apiKey.trim();
        if (trimmedKey.length > 0) {
          await setGatewayApiKey(trimmedKey);
          setHasApiKey(true);
        }
        // Wake gateway-gated surfaces immediately (model strip, capabilities,
        // status pills) — otherwise they stay degraded until a manual reload.
        notifyGatewayConfigChanged();
      } else if (mode === "standalone") {
        // Standalone: store provider keys through the same OS key vault the
        // Settings → Providers tab uses. Gateway config is never touched.
        const anthropic = anthropicKey.trim();
        const openai = openaiKey.trim();
        if (anthropic.length > 0) {
          await setProviderKey("anthropic", anthropic);
          setAnthropicKey("");
        }
        if (openai.length > 0) {
          await setProviderKey("openai", openai);
          setOpenaiKey("");
        }
      }
      // mode === "cli": the CLIs carry their own sign-in; nothing to persist.
      applyTheme(theme);
      await showNextSteps();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setSaving(false);
    }
  }

  function next() {
    if (step === 3) {
      void saveAndLaunch();
      return;
    }
    if (step < 3) setStep((step + 1) as StepIndex);
  }

  function back() {
    if (step === 0 || step === 4) return;
    setStep((step - 1) as StepIndex);
  }

  const isLast = step === 3;
  const isSummary = step === 4;
  // A typed-but-nonexistent path blocks Next on the vault step — same gate as
  // Setup's "Connect vault". Blank stays allowed (the step is optional).
  const vaultBlocks =
    step === 2 &&
    vaultPath.trim().length > 0 &&
    vaultInfo !== null &&
    !vaultInfo.is_valid;

  const agentsReady = nextSteps ? nextSteps.models + nextSteps.clis.length : 0;
  const summaryTitle =
    nextSteps &&
    (agentsReady === 0 || !nextSteps.vault || nextSteps.projects === 0)
      ? "Saved — a few things to finish"
      : "You're all set";

  return (
    <div
      className="modal-backdrop onboarding-wizard"
      role="dialog"
      aria-modal="true"
    >
      <div
        className="modal onboarding-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="onboarding-step-label">
          {isSummary ? "Setup complete" : `Step ${step + 1} of 4`}
        </div>
        <h2 className="onboarding-title">
          {isSummary ? summaryTitle : stepTitle(step, mode)}
        </h2>

        {step === 0 && (
          <div className="onboarding-step">
            <p>
              Cortex is your central brain — it chats with your models, indexes
              your notes, and orchestrates coding agents.
            </p>
            <div className="onboarding-subhead">Agents on this machine</div>
            <CliList
              clis={clis}
              onInstall={openInstallUrl}
              emptyHint="No agent CLI found — install Claude Code, Codex or Gemini CLI, or pick a gateway / API keys below."
            />
            <p>How should Cortex reach its models?</p>
            <div
              className="onboarding-mode-choice"
              role="radiogroup"
              aria-label="Connection mode"
            >
              <label
                className={`onboarding-mode-row ${mode === "cli" ? "selected" : ""}`}
              >
                <input
                  type="radio"
                  name="cortex-connect-mode"
                  value="cli"
                  checked={mode === "cli"}
                  onChange={() => pickMode("cli")}
                />
                <div className="onboarding-mode-meta">
                  <strong>Use the agent CLIs installed here</strong>
                  <small>
                    {installedClis.length > 0
                      ? `Drive ${installedClis.map((c) => c.label).join(", ")} directly — no server, no extra keys.`
                      : "Drive Claude Code, Codex, Gemini CLI or Aider directly — nothing found yet, install one first."}
                  </small>
                </div>
              </label>
              <label
                className={`onboarding-mode-row ${mode === "gateway" ? "selected" : ""}`}
              >
                <input
                  type="radio"
                  name="cortex-connect-mode"
                  value="gateway"
                  checked={mode === "gateway"}
                  onChange={() => pickMode("gateway")}
                />
                <div className="onboarding-mode-meta">
                  <strong>Connect to a Cortex Gateway</strong>
                  <small>
                    Route everything through a Cortex Gateway server you run —
                    agents, models, and usage live on the gateway.
                  </small>
                </div>
              </label>
              <label
                className={`onboarding-mode-row ${mode === "standalone" ? "selected" : ""}`}
              >
                <input
                  type="radio"
                  name="cortex-connect-mode"
                  value="standalone"
                  checked={mode === "standalone"}
                  onChange={() => pickMode("standalone")}
                />
                <div className="onboarding-mode-meta">
                  <strong>Use API keys directly (standalone)</strong>
                  <small>
                    No gateway — talk to Anthropic / OpenAI with your own keys,
                    stored encrypted in the OS key vault.
                  </small>
                </div>
              </label>
            </div>
            <label className="onboarding-theme-inline">
              <span>Theme</span>
              <select
                value={theme}
                onChange={(e) => {
                  const t = e.target.value as ThemeId;
                  setTheme(t);
                  applyTheme(t);
                }}
                aria-label="Theme"
              >
                {THEMES.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.label}
                  </option>
                ))}
              </select>
            </label>
            <p className="onboarding-muted">
              This quick tour takes about 30 seconds. You can skip any step and
              tweak everything later in Settings.
            </p>
          </div>
        )}

        {step === 1 && (
          <div className="onboarding-step">
            <p>
              Agents work inside a project — any git repository on this machine.
              Pick one now and Cortex opens a chat with its context (CLAUDE.md,
              runbooks, memory) already loaded.
            </p>
            <div className="onboarding-project-pick">
              <button
                type="button"
                className="btn-primary onboarding-pick-btn"
                onClick={() => void pickProject()}
                disabled={picking || saving}
              >
                <FolderPlus size={14} strokeWidth={1.75} aria-hidden="true" />{" "}
                {picking ? "Opening…" : "Open a project folder…"}
              </button>
              {activeProject?.kind === "code" && (
                <div className="onboarding-next-step ok">
                  <CheckCircle2
                    size={16}
                    strokeWidth={1.75}
                    className="onboarding-next-step-icon ok"
                    aria-hidden="true"
                  />
                  <div className="onboarding-next-step-body">
                    <span className="onboarding-next-step-title">
                      {activeProject.name}
                    </span>
                    <span className="onboarding-next-step-hint">
                      {activeProject.root}
                    </span>
                  </div>
                </div>
              )}
              {pickError && (
                <div
                  className="onboarding-muted onboarding-pick-error"
                  role="alert"
                >
                  {pickError}
                </div>
              )}
            </div>
            <div className="onboarding-muted">
              {projectCount > 0
                ? `${projectCount} project${projectCount === 1 ? "" : "s"} already discovered in your projects folder — you can also pick one from the sidebar later.`
                : "Optional — Cortex also lists every repository under ~/projects/ (or CORTEX_PROJECTS_ROOT)."}
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="onboarding-step">
            <p>
              Point Cortex at your Obsidian vault so the Brain panel can surface
              notes during chat.
            </p>
            <VaultField
              value={vaultPath}
              onChange={setVaultPath}
              onValidation={setVaultInfo}
              disabled={saving}
            />
            <div className="onboarding-muted">
              Optional — leave blank to connect one later from the Setup tab.
            </div>
          </div>
        )}

        {step === 3 && mode === "cli" && (
          <div className="onboarding-step">
            <p>
              {installedClis.length > 0
                ? "These agents are installed and Cortex will drive them directly. Anything not signed in yet can be fixed from Settings → Providers → Sign in."
                : "No agent CLI was found. Install one from the links below, then sign in — or go back and pick a gateway or API keys."}
            </p>
            <CliList
              clis={clis}
              onInstall={openInstallUrl}
              emptyHint="No agent CLI found on this machine."
              showAll
            />
            <div className="onboarding-muted">
              You can add a Cortex Gateway or provider API keys any time in
              Settings → Providers; local agents keep working alongside them.
            </div>
          </div>
        )}

        {step === 3 && mode === "gateway" && (
          <div className="onboarding-step">
            <p>
              The gateway runs your agents. Optional — leave blank if you
              don&apos;t use a Cortex Gateway (you can connect one later in
              Settings).
            </p>
            <label>
              Gateway base URL
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://gateway.example.com:8642"
              />
            </label>
            <label>
              Model id
              <input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gateway-agent"
              />
            </label>
            <label>
              API key (optional)
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="Bearer token for /v1/* access"
              />
            </label>
            <div className="onboarding-muted">
              Tip: any field left empty keeps its existing value — you can
              change all of this later in Settings.
            </div>
          </div>
        )}

        {step === 3 && mode === "standalone" && (
          <div className="onboarding-step">
            <p>
              Add a key for at least one provider. Keys are stored encrypted in
              the OS key vault and never leave this machine.
            </p>
            <label>
              Anthropic API key
              <input
                type="password"
                value={anthropicKey}
                onChange={(e) => setAnthropicKey(e.target.value)}
                placeholder={
                  providerCfg?.anthropic_key_set
                    ? "leave blank to keep current"
                    : "sk-ant-…"
                }
              />
            </label>
            <label>
              OpenAI API key
              <input
                type="password"
                value={openaiKey}
                onChange={(e) => setOpenaiKey(e.target.value)}
                placeholder={
                  providerCfg?.openai_key_set
                    ? "leave blank to keep current"
                    : "sk-…"
                }
              />
            </label>
            {providerCfg && !providerCfg.standalone_build && (
              <div className="onboarding-muted">
                <strong>Note:</strong> this build was compiled without the
                standalone adapters — keys are saved, but direct providers only
                activate in a build with the <code>standalone</code> feature.
              </div>
            )}
            <div className="onboarding-muted">
              Optional — you can add or rotate keys any time from Settings →
              Providers.
            </div>
          </div>
        )}

        {isSummary && nextSteps && (
          <div className="onboarding-step onboarding-next-steps">
            <NextStepRow
              ok={agentsReady > 0}
              title={
                agentsReady > 0
                  ? `${agentsReady} agent${agentsReady === 1 ? "" : "s"} ready to chat`
                  : "No agents available yet"
              }
              hint={
                nextSteps.clis.length > 0
                  ? nextSteps.clis.join(", ") +
                    (nextSteps.models > 0
                      ? ` · ${nextSteps.models} model${nextSteps.models === 1 ? "" : "s"}`
                      : "")
                  : agentsReady > 0
                    ? undefined
                    : "Install an agent CLI (Claude Code, Codex, Gemini), pull a local model or connect a provider in the Cookbook."
              }
              actionLabel={agentsReady > 0 ? undefined : "Open Cookbook"}
              onAction={() => finishInto("cookbook")}
            />
            <NextStepRow
              ok={Boolean(nextSteps.vault)}
              title={
                nextSteps.vault
                  ? "Obsidian vault connected"
                  : "No vault connected"
              }
              hint={
                nextSteps.vault ??
                "Link your notes from the Setup tab so the Brain panel can surface them."
              }
              actionLabel={nextSteps.vault ? undefined : "Open Setup"}
              onAction={() => finishInto("setup")}
            />
            <NextStepRow
              ok={nextSteps.projects > 0}
              title={
                nextSteps.projects > 0
                  ? `${nextSteps.projects} project${nextSteps.projects === 1 ? "" : "s"} discovered`
                  : "No projects yet"
              }
              hint={
                nextSteps.projects > 0
                  ? "Pick one from the Projects sidebar to load its context into chat."
                  : "Add any git repository as a project — or clone one from the Setup tab."
              }
              actionLabel={
                nextSteps.projects > 0
                  ? "Browse projects"
                  : "Add a project folder"
              }
              onAction={() => {
                if (nextSteps.projects > 0) {
                  finishInto("projects");
                  return;
                }
                // Picker first, then close: the wizard would otherwise vanish
                // under the native dialog and the result would go unseen.
                void addProjectViaDialog().finally(finish);
              }}
            />
          </div>
        )}

        {err && <div style={{ color: "var(--danger)" }}>{err}</div>}

        <div className="onboarding-nav">
          {!isSummary && (
            <button
              className="onboarding-skip link-btn"
              onClick={skipAll}
              disabled={saving}
            >
              Skip setup
            </button>
          )}
          <div className="onboarding-nav-right">
            {!isSummary && (
              <button onClick={back} disabled={step === 0 || saving}>
                Back
              </button>
            )}
            <button
              className="onboarding-next"
              onClick={isSummary ? finish : next}
              disabled={saving || vaultBlocks}
              title={
                vaultBlocks
                  ? "That folder doesn't exist — fix the path or leave it blank"
                  : undefined
              }
            >
              {saving
                ? "Saving…"
                : isSummary
                  ? "Start chatting"
                  : isLast
                    ? "Save & launch"
                    : "Next"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * Installed-CLI list: one row per agent CLI with install + sign-in status.
 * `null` = probe in flight. By default only installed CLIs are listed (the
 * welcome card stays short); `showAll` also lists missing ones with their
 * install link (the "Local agents ready" step).
 */
function CliList({
  clis,
  onInstall,
  emptyHint,
  showAll = false,
}: {
  clis: LocalCliProvider[] | null;
  onInstall: (url: string) => void;
  emptyHint: string;
  showAll?: boolean;
}) {
  if (clis === null) {
    return <div className="onboarding-muted">Looking for agent CLIs…</div>;
  }
  const rows = showAll ? clis : clis.filter((c) => c.installed);
  if (rows.length === 0) {
    return <div className="onboarding-muted">{emptyHint}</div>;
  }
  return (
    <div className="onboarding-next-steps onboarding-cli-list">
      {rows.map((c) => {
        const ok = c.installed && c.authenticated !== false;
        const status = !c.installed
          ? "not installed"
          : c.authenticated === true
            ? "signed in"
            : c.authenticated === false
              ? c.login_cmd
                ? `not signed in — run \`${c.login_cmd}\``
                : "not signed in"
              : "installed";
        return (
          <div
            key={c.id}
            className={`onboarding-next-step ${ok ? "ok" : "todo"}`}
          >
            {ok ? (
              <CheckCircle2
                size={16}
                strokeWidth={1.75}
                className="onboarding-next-step-icon ok"
                aria-hidden="true"
              />
            ) : (
              <CircleDashed
                size={16}
                strokeWidth={1.75}
                className="onboarding-next-step-icon todo"
                aria-hidden="true"
              />
            )}
            <div className="onboarding-next-step-body">
              <span className="onboarding-next-step-title">{c.label}</span>
              <span className="onboarding-next-step-hint">
                {status}
                {!c.installed && c.install_hint ? ` · ${c.install_hint}` : ""}
              </span>
            </div>
            {!c.installed && c.install_url && (
              <button
                type="button"
                className="setup-btn onboarding-next-step-action"
                onClick={() => onInstall(c.install_url)}
              >
                Install
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** One row of the final card: a real state check + the tab that fixes it. */
function NextStepRow({
  ok,
  title,
  hint,
  actionLabel,
  onAction,
}: {
  ok: boolean;
  title: string;
  hint?: string;
  actionLabel?: string;
  onAction: () => void;
}) {
  return (
    <div className={`onboarding-next-step ${ok ? "ok" : "todo"}`}>
      {ok ? (
        <CheckCircle2
          size={16}
          strokeWidth={1.75}
          className="onboarding-next-step-icon ok"
          aria-hidden="true"
        />
      ) : (
        <CircleDashed
          size={16}
          strokeWidth={1.75}
          className="onboarding-next-step-icon todo"
          aria-hidden="true"
        />
      )}
      <div className="onboarding-next-step-body">
        <span className="onboarding-next-step-title">{title}</span>
        {hint && <span className="onboarding-next-step-hint">{hint}</span>}
      </div>
      {actionLabel && (
        <button
          className="setup-btn onboarding-next-step-action"
          onClick={onAction}
        >
          {actionLabel}
        </button>
      )}
    </div>
  );
}
