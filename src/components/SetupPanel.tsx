/**
 * Self-setup wizard panel: point Cortex at an Obsidian vault, or clone/connect
 * a git server (Gitea/GitHub) entirely from inside the app.
 *
 * Two sections:
 *   A. Obsidian vault — pick/type a path, get live ✓/✗ validation (does it
 *      exist? does it have a `.obsidian` folder?), then "Connect Vault" which
 *      persists via `setObsidianVault`.
 *   B. Git server — either *clone* a remote URL into a target dir, or *connect*
 *      an already-cloned local repo. Clone streams the tail of git's output.
 *
 * All backend calls go through `@/lib/cortex-bridge`. Config is persisted by the
 * Rust side (keychain-free; ~/.cortex/git-config.json + the obsidian setter).
 */

import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Brain, GitBranch } from "lucide-react";
import { setObsidianVault } from "@/lib/brain";
import {
  cloneGitRepo,
  getGatewayConfig,
  setGitServerClonedPath,
  validateGitUrl,
  type GitUrlInfo,
  type VaultInfo,
} from "@/lib/cortex-bridge";
import { humanizeError } from "@/lib/errors";
import { openProjectByPath } from "@/lib/open-project";
import { pushToast } from "@/lib/toast";
import { VaultField } from "./VaultField";
import "../styles/setup.css";

type GitMode = "clone" | "connect";

export function SetupPanel() {
  // --- Obsidian vault state ---
  const [vaultPath, setVaultPath] = useState("");
  const [vaultInfo, setVaultInfo] = useState<VaultInfo | null>(null);
  const [vaultBusy, setVaultBusy] = useState(false);
  const [currentVault, setCurrentVault] = useState<string | null>(null);

  // --- Git server state ---
  const [gitMode, setGitMode] = useState<GitMode>("clone");
  const [gitUrl, setGitUrl] = useState("");
  const [gitUrlInfo, setGitUrlInfo] = useState<GitUrlInfo | null>(null);
  const [targetDir, setTargetDir] = useState("");
  const [connectPath, setConnectPath] = useState("");
  const [gitBusy, setGitBusy] = useState(false);
  const [gitOutput, setGitOutput] = useState("");
  const [currentGitUrl, setCurrentGitUrl] = useState<string | null>(null);
  const [currentGitPath, setCurrentGitPath] = useState<string | null>(null);

  // Load whatever's already configured so the panel reflects current state.
  useEffect(() => {
    getGatewayConfig()
      .then((cfg) => {
        setCurrentVault(cfg.obsidian_vault);
        setCurrentGitUrl(cfg.git_server_url);
        setCurrentGitPath(cfg.git_server_cloned_path);
        if (!vaultPath && cfg.obsidian_vault) setVaultPath(cfg.obsidian_vault);
        if (!gitUrl && cfg.git_server_url) setGitUrl(cfg.git_server_url);
      })
      .catch(() => {
        /* non-fatal: panel still usable with blank state */
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Debounced live validation of the git URL.
  useEffect(() => {
    const u = gitUrl.trim();
    if (!u) {
      setGitUrlInfo(null);
      return;
    }
    const id = setTimeout(() => {
      validateGitUrl(u)
        .then(setGitUrlInfo)
        .catch(() => setGitUrlInfo(null));
    }, 300);
    return () => clearTimeout(id);
  }, [gitUrl]);

  async function browse(setter: (v: string) => void, title: string) {
    try {
      const selected = await openDialog({ directory: true, multiple: false, title });
      if (typeof selected === "string" && selected.length > 0) setter(selected);
    } catch (e) {
      pushToast({ title: "Couldn't open picker", body: humanizeError(e), kind: "error" });
    }
  }

  async function connectVault() {
    const p = vaultPath.trim();
    if (!p) return;
    setVaultBusy(true);
    try {
      await setObsidianVault(p);
      setCurrentVault(p);
      pushToast({ title: "Vault connected", body: p, kind: "success" });
    } catch (e) {
      pushToast({ title: "Couldn't connect vault", body: humanizeError(e), kind: "error" });
    } finally {
      setVaultBusy(false);
    }
  }

  async function doClone() {
    const url = gitUrl.trim();
    const dir = targetDir.trim();
    if (!url || !dir) return;
    setGitBusy(true);
    setGitOutput("Cloning…");
    try {
      const res = await cloneGitRepo(url, dir);
      const out = [res.stdout_tail, res.stderr_tail].filter(Boolean).join("\n").trim();
      setGitOutput(out || (res.ok ? "Clone complete." : `Exit ${res.exit_code}`));
      if (res.ok) {
        setCurrentGitUrl(url);
        // Prefer the canonical path the backend registered the project
        // under — "Open project" matches list_projects rows against it.
        setCurrentGitPath(res.project_root ?? dir);
        pushToast({
          title: "Repository cloned",
          body: "Registered as a project — use Open project to jump in.",
          kind: "success",
        });
      } else {
        pushToast({
          title: "Clone failed",
          body: res.stderr_tail || `git exited ${res.exit_code}`,
          kind: "error",
        });
      }
    } catch (e) {
      setGitOutput(humanizeError(e));
      pushToast({ title: "Clone failed", body: humanizeError(e), kind: "error" });
    } finally {
      setGitBusy(false);
    }
  }

  async function connectExisting() {
    const p = connectPath.trim();
    if (!p) return;
    setGitBusy(true);
    try {
      const canonical = await setGitServerClonedPath(p);
      setCurrentGitPath(canonical);
      pushToast({
        title: "Repository connected",
        body: "Registered as a project — use Open project to jump in.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Couldn't connect repo", body: humanizeError(e), kind: "error" });
    } finally {
      setGitBusy(false);
    }
  }

  /** Hand off into the connected repo: make sure it's registered (covers a
   *  path persisted before registration existed), then activate it and reveal
   *  the Projects sidebar — the same flow as clicking its sidebar row. */
  async function openConnected() {
    const p = currentGitPath;
    if (!p) return;
    setGitBusy(true);
    try {
      const canonical = await setGitServerClonedPath(p);
      if (canonical !== p) setCurrentGitPath(canonical);
      const opened = await openProjectByPath(canonical);
      if (!opened) {
        pushToast({
          title: "Couldn't open project",
          body: `${canonical} didn't show up in the project list.`,
          kind: "error",
        });
      }
    } catch (e) {
      pushToast({ title: "Couldn't open project", body: humanizeError(e), kind: "error" });
    } finally {
      setGitBusy(false);
    }
  }

  return (
    <div className="setup-panel">
      {/* --- Section A: Obsidian vault --- */}
      <section className="setup-section">
        <div className="setup-section-head">
          <Brain size={16} strokeWidth={1.75} aria-hidden="true" />
          <span className="setup-section-title">Obsidian vault</span>
        </div>
        <p className="setup-section-desc">
          Point Cortex at your Obsidian vault folder so the Brain panel can
          surface notes during chat.
        </p>
        {currentVault && (
          <div className="setup-current">
            Connected: <code>{currentVault}</code>
          </div>
        )}
        <VaultField
          value={vaultPath}
          onChange={setVaultPath}
          onValidation={setVaultInfo}
          disabled={vaultBusy}
        />
        <div className="setup-input-row">
          <button
            className="setup-btn primary"
            onClick={() => void connectVault()}
            disabled={vaultBusy || !vaultPath.trim() || (vaultInfo ? !vaultInfo.is_valid : false)}
          >
            {vaultBusy ? "Connecting…" : "Connect vault"}
          </button>
        </div>
      </section>

      {/* --- Section B: Git server --- */}
      <section className="setup-section">
        <div className="setup-section-head">
          <GitBranch size={16} strokeWidth={1.75} aria-hidden="true" />
          <span className="setup-section-title">Git server</span>
        </div>
        <p className="setup-section-desc">
          Clone a repository from a Gitea/GitHub URL, or connect a repo you've
          already cloned locally.
        </p>
        {(currentGitUrl || currentGitPath) && (
          <div className="setup-current">
            {currentGitUrl && (
              <>
                URL: <code>{redactCredentials(currentGitUrl)}</code>
                <br />
              </>
            )}
            {currentGitPath && (
              <>
                Path: <code>{currentGitPath}</code>
              </>
            )}
          </div>
        )}
        {currentGitPath && (
          <div className="setup-input-row">
            <button
              className="setup-btn primary"
              onClick={() => void openConnected()}
              disabled={gitBusy}
              title="Set as the active project and show it in the Projects sidebar"
            >
              Open project
            </button>
            <span className="setup-section-desc setup-open-hint">
              Activates the repo, loads its context into chat, and shows it in
              the Projects sidebar.
            </span>
          </div>
        )}

        <div className="setup-radio-row">
          <label>
            <input
              type="radio"
              name="git-mode"
              checked={gitMode === "clone"}
              onChange={() => setGitMode("clone")}
            />
            Clone a repo
          </label>
          <label>
            <input
              type="radio"
              name="git-mode"
              checked={gitMode === "connect"}
              onChange={() => setGitMode("connect")}
            />
            Connect existing
          </label>
        </div>

        {gitMode === "clone" ? (
          <>
            <div className="setup-field">
              <span className="setup-field-label">Repository URL</span>
              <input
                className="setup-input"
                value={gitUrl}
                onChange={(e) => setGitUrl(e.target.value)}
                placeholder="https://github.com/owner/repo.git"
              />
            </div>
            {gitUrlInfo && <GitUrlStatus info={gitUrlInfo} />}
            <div className="setup-field">
              <span className="setup-field-label">Clone into</span>
              <div className="setup-input-row">
                <input
                  className="setup-input"
                  value={targetDir}
                  onChange={(e) => setTargetDir(e.target.value)}
                  placeholder="~/projects/repo"
                />
                <button
                  className="setup-btn"
                  onClick={() => void browse(setTargetDir, "Select clone target folder")}
                  disabled={gitBusy}
                >
                  Browse…
                </button>
              </div>
            </div>
            <p className="setup-section-desc">
              Tip: prefer SSH keys or a GitHub App over embedding a token in the
              URL — tokens in URLs can be recovered from history.
            </p>
            <div className="setup-input-row">
              <button
                className="setup-btn primary"
                onClick={() => void doClone()}
                disabled={
                  gitBusy ||
                  !targetDir.trim() ||
                  !gitUrl.trim() ||
                  (gitUrlInfo ? !gitUrlInfo.is_valid : false)
                }
              >
                {gitBusy ? "Cloning…" : "Clone & connect"}
              </button>
            </div>
            {gitOutput && <pre className="setup-output">{gitOutput}</pre>}
          </>
        ) : (
          <>
            <div className="setup-field">
              <span className="setup-field-label">Local repository path</span>
              <div className="setup-input-row">
                <input
                  className="setup-input"
                  value={connectPath}
                  onChange={(e) => setConnectPath(e.target.value)}
                  placeholder="~/projects/repo"
                />
                <button
                  className="setup-btn"
                  onClick={() => void browse(setConnectPath, "Select git repository folder")}
                  disabled={gitBusy}
                >
                  Browse…
                </button>
              </div>
            </div>
            <div className="setup-input-row">
              <button
                className="setup-btn primary"
                onClick={() => void connectExisting()}
                disabled={gitBusy || !connectPath.trim()}
              >
                {gitBusy ? "Connecting…" : "Connect existing"}
              </button>
            </div>
          </>
        )}
      </section>
    </div>
  );
}

function GitUrlStatus({ info }: { info: GitUrlInfo }) {
  if (!info.is_valid) {
    return <span className="setup-status bad">✗ Invalid URL format</span>;
  }
  return (
    <span className="setup-status ok">
      ✓ Valid{info.hostname ? ` (${info.hostname})` : ""}
    </span>
  );
}

/** Hide an embedded `user:token@` from a displayed URL so we never echo
 *  credentials back to the screen. */
function redactCredentials(url: string): string {
  return url.replace(/\/\/[^/@]+@/, "//•••@");
}
