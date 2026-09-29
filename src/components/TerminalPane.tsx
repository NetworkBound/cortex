// Embedded PTY pane.
//
// Mounts a single xterm.js Terminal, opens a backend PTY on mount, wires
// bidirectional bytes, and tears it down on unmount. v1 is intentionally
// scope-limited to one terminal session — no tabs, no buffer persistence.
//
// Working directory: `terminal_open` has no cwd parameter (the PTY always
// starts in $HOME), so when a project is active the pane sends the shell one
// `cd` line right after it opens — the same trick IDE terminals use — and the
// toolbar offers "cd project" to jump back plus "Terminal here" to pop an
// external terminal window in the project root (lib/open-external).

import { useEffect, useRef, useState } from "react";
import { SquareTerminal } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import {
  describeOpenError,
  openTerminalHere,
  shellCdLine,
} from "@/lib/open-external";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import {
  closeTerminal,
  onTerminalClosed,
  onTerminalOutput,
  openTerminal,
  resizeTerminal,
  writeTerminal,
} from "@/lib/terminal";
import { THEME_CHANGED_EVENT } from "@/lib/theme-engine";
import "../styles/terminal-pane.css";

/** Build the xterm theme from the active app theme's CSS custom properties.
 *
 *  xterm paints to its own renderer, so unlike DOM surfaces it can't consume
 *  `var(--token)` references — we resolve the tokens at call time and hand it
 *  concrete colors. Re-invoked on every THEME_CHANGED_EVENT so the live
 *  terminal (a keep-alive pane that survives tab switches and theme flips)
 *  re-colors in place. The hex fallbacks mirror the global.css dark defaults.
 */
function xtermThemeFromTokens(): ITheme {
  const styles =
    typeof window !== "undefined"
      ? getComputedStyle(document.documentElement)
      : null;
  const token = (name: string, fallback: string): string => {
    const v = styles?.getPropertyValue(name).trim();
    return v || fallback;
  };
  const background = token("--bg-sunken", "#0b0f14");
  return {
    background,
    foreground: token("--text", "#e6e6e6"),
    cursor: token("--accent", "#fb923c"),
    cursorAccent: background,
    // rgba() strings are valid xterm colors — the selection keeps the text
    // underneath legible at any theme's accent.
    selectionBackground: token("--accent-glow", "rgba(251, 146, 60, 0.25)"),
    selectionInactiveBackground: token(
      "--accent-soft",
      "rgba(251, 146, 60, 0.10)",
    ),
  };
}

export function TerminalPane() {
  const hostRef = useRef<HTMLDivElement>(null);
  const projectRoot = useCortexStore((s) => s.activeProject?.root ?? null);
  const [cwdLabel, setCwdLabel] = useState<string | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const idRef = useRef<string | null>(null);
  const [status, setStatus] = useState<
    "booting" | "ready" | "closed" | "error"
  >("booting");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!hostRef.current) return;
    let disposed = false;
    let unlistenOutput: UnlistenFn | null = null;
    let unlistenClosed: UnlistenFn | null = null;

    // 1. Initialize xterm with sensible defaults. Colors come from the active
    //    theme's tokens so the shell matches the rest of the app (and keeps
    //    matching — see the THEME_CHANGED_EVENT listener below).
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
      /* renderer not ready yet (e.g. StrictMode double-mount) — the
         ResizeObserver below refits as soon as real dimensions exist */
    }
    termRef.current = term;
    fitRef.current = fit;

    const { cols, rows } = term;

    // 2. Open the backend PTY and wire byte streams.
    (async () => {
      try {
        const handle = await openTerminal(cols, rows);
        if (disposed) {
          await closeTerminal(handle.id);
          return;
        }
        idRef.current = handle.id;

        unlistenOutput = await onTerminalOutput(handle.id, (chunk) => {
          term.write(chunk);
        });
        unlistenClosed = await onTerminalClosed(handle.id, () => {
          setStatus("closed");
        });
        if (disposed) {
          // Unmounted while the subscriptions were in flight — the cleanup
          // already ran with them still null, so release everything here.
          unlistenOutput();
          unlistenClosed();
          idRef.current = null;
          await closeTerminal(handle.id);
          return;
        }

        // User keystrokes → backend.
        term.onData((data) => {
          if (!idRef.current) return;
          void writeTerminal(idRef.current, data).catch((e) => {
            console.warn("terminal write failed", e);
          });
        });
        setStatus("ready");

        // Start in the active project (see the module comment). Read the
        // store imperatively: this effect runs once on mount by design and
        // must not re-run (and re-spawn the shell) when the project changes.
        const root = useCortexStore.getState().activeProject?.root;
        if (root) {
          void writeTerminal(handle.id, shellCdLine(root))
            .then(() => setCwdLabel(root))
            .catch(() => {});
        }
      } catch (e) {
        setStatus("error");
        setError(humanizeError(e));
      }
    })();

    // 2b. Track app theme switches live. The pane is keep-alive mounted, so a
    //     theme change never remounts it — re-resolve the tokens and hand the
    //     existing instance a fresh theme object (xterm repaints on options
    //     assignment). The buffer, scrollback, and PTY are untouched.
    function onThemeChanged() {
      const t = termRef.current;
      if (!t) return;
      try {
        t.options.theme = xtermThemeFromTokens();
      } catch {
        /* an unparseable token value (e.g. a malformed custom theme) — keep
           the previous colors rather than crash the shell */
      }
    }
    window.addEventListener(THEME_CHANGED_EVENT, onThemeChanged);

    // 3. Resize observer keeps the PTY in sync with the container.
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
        const id = idRef.current;
        if (id) void resizeTerminal(id, term.cols, term.rows).catch(() => {});
      } catch {
        /* ignore mid-mount races */
      }
    });
    ro.observe(hostRef.current);

    return () => {
      disposed = true;
      window.removeEventListener(THEME_CHANGED_EVENT, onThemeChanged);
      ro.disconnect();
      if (unlistenOutput) unlistenOutput();
      if (unlistenClosed) unlistenClosed();
      const id = idRef.current;
      idRef.current = null;
      if (id) void closeTerminal(id).catch(() => {});
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, []);

  /** "cd project": type a cd into the live shell for the current project. */
  function cdToProject() {
    const id = idRef.current;
    if (!id || !projectRoot) return;
    void writeTerminal(id, shellCdLine(projectRoot))
      .then(() => {
        setCwdLabel(projectRoot);
        termRef.current?.focus();
      })
      .catch((e) =>
        pushToast({
          title: "Couldn't change directory",
          body: humanizeError(e),
          kind: "error",
        }),
      );
  }

  /** "Terminal here": an external terminal window in the project root. */
  function externalTerminal() {
    if (!projectRoot) return;
    openTerminalHere(projectRoot)
      .then((program) =>
        pushToast({
          title: "Terminal opened",
          body: `${program} · ${projectRoot}`,
          kind: "success",
        }),
      )
      .catch((e) =>
        pushToast({
          title: "Couldn't open terminal",
          body: describeOpenError(e),
          kind: "error",
        }),
      );
  }

  return (
    <div className="terminal-pane">
      <div className="terminal-pane-toolbar">
        <span
          className="terminal-pane-cwd"
          title={cwdLabel ?? projectRoot ?? "home directory"}
        >
          {cwdLabel ?? projectRoot ?? "~"}
        </span>
        <button
          type="button"
          className="btn-ghost terminal-pane-tool"
          onClick={cdToProject}
          disabled={!projectRoot || status !== "ready"}
          title={
            projectRoot
              ? `cd into ${projectRoot}`
              : "Pick an active project first"
          }
        >
          cd project
        </button>
        <button
          type="button"
          className="btn-ghost terminal-pane-tool"
          onClick={externalTerminal}
          disabled={!projectRoot}
          title={
            projectRoot
              ? `Open a system terminal window in ${projectRoot}`
              : "Pick an active project first"
          }
        >
          <SquareTerminal size={13} strokeWidth={1.75} aria-hidden="true" />{" "}
          Terminal here
        </button>
      </div>
      <div className="terminal-pane-host" ref={hostRef} />
      {status === "booting" && (
        <div className="terminal-pane-status muted">starting shell…</div>
      )}
      {status === "closed" && (
        <div className="terminal-pane-status muted">shell exited.</div>
      )}
      {status === "error" && (
        <div className="terminal-pane-status error">
          terminal failed: {error ?? "unknown error"}
        </div>
      )}
    </div>
  );
}
