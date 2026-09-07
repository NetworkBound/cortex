// In-app CLI sign-in terminal.
//
// Renders an xterm.js view attached to a PTY that the backend already spawned
// running a provider CLI's own login command (`claude /login`, `codex login`,
// …). The user completes the provider's OAuth / device-code / key prompt right
// inside Cortex; on close we tear the PTY down. Reuses the same terminal byte
// plumbing as the embedded TerminalPane.

import { useEffect, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import {
  closeTerminal,
  onTerminalClosed,
  onTerminalOutput,
  resizeTerminal,
  writeTerminal,
} from "@/lib/terminal";
import { cliProviderLogin } from "@/lib/cortex-bridge";

/** Resolve the active theme's tokens to concrete xterm colors (xterm paints to
 *  its own renderer and can't consume `var(--token)`). */
function xtermThemeFromTokens(): ITheme {
  const styles =
    typeof window !== "undefined" ? getComputedStyle(document.documentElement) : null;
  const token = (name: string, fallback: string): string =>
    styles?.getPropertyValue(name).trim() || fallback;
  const background = token("--bg-sunken", "#0b0f14");
  return {
    background,
    foreground: token("--text", "#e6e6e6"),
    cursor: token("--accent", "#fb923c"),
    cursorAccent: background,
  };
}

export function CliLoginModal({
  providerId,
  providerLabel,
  loginCmd,
  onClose,
}: {
  providerId: string;
  providerLabel: string;
  loginCmd: string;
  onClose: () => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const idRef = useRef<string | null>(null);
  const [status, setStatus] = useState<"booting" | "ready" | "closed" | "error">("booting");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!hostRef.current) return;
    let disposed = false;
    let unlistenOutput: UnlistenFn | null = null;
    let unlistenClosed: UnlistenFn | null = null;

    const term = new Terminal({
      fontFamily: "var(--mono, ui-monospace, Menlo, monospace)",
      fontSize: 13,
      cursorBlink: true,
      convertEol: false,
      allowProposedApi: true,
      theme: xtermThemeFromTokens(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    try {
      fit.fit();
    } catch {
      /* renderer not ready — ResizeObserver refits below */
    }
    termRef.current = term;
    fitRef.current = fit;

    const { cols, rows } = term;

    (async () => {
      try {
        // Backend spawns the provider's login command in a PTY and hands back
        // its id; from here it's identical to the embedded terminal.
        const handle = await cliProviderLogin(providerId, cols, rows);
        if (disposed) {
          await closeTerminal(handle.id);
          return;
        }
        idRef.current = handle.id;

        unlistenOutput = await onTerminalOutput(handle.id, (chunk) => term.write(chunk));
        unlistenClosed = await onTerminalClosed(handle.id, () => setStatus("closed"));

        term.onData((data) => {
          if (!idRef.current) return;
          void writeTerminal(idRef.current, data).catch((e) =>
            console.warn("login terminal write failed", e),
          );
        });
        setStatus("ready");
      } catch (e) {
        setStatus("error");
        setError(humanizeError(e));
      }
    })();

    const ro = new ResizeObserver(() => {
      const f = fitRef.current;
      const t = termRef.current;
      if (!f || !t) return;
      try {
        f.fit();
        if (idRef.current) void resizeTerminal(idRef.current, t.cols, t.rows);
      } catch {
        /* container not measurable yet */
      }
    });
    ro.observe(hostRef.current);

    return () => {
      disposed = true;
      ro.disconnect();
      unlistenOutput?.();
      unlistenClosed?.();
      if (idRef.current) void closeTerminal(idRef.current);
      term.dispose();
    };
  }, [providerId]);

  return (
    <div className="cli-login-backdrop" role="dialog" aria-modal="true">
      <div className="cli-login-modal">
        <div className="cli-login-head">
          <div>
            <strong>Sign in to {providerLabel}</strong>
            <div className="settings-muted">
              Running <code>{loginCmd}</code>. Follow the prompts below (a browser
              may open for OAuth). Close when done.
            </div>
          </div>
          <button type="button" onClick={onClose}>
            {status === "closed" ? "Done" : "Close"}
          </button>
        </div>
        {error && <div className="settings-err">{error}</div>}
        <div className="cli-login-term" ref={hostRef} />
        {status === "closed" && (
          <div className="settings-muted">
            The login process exited. If sign-in succeeded, the status pill will
            update — reopen Settings or click Refresh.
          </div>
        )}
      </div>
    </div>
  );
}
