/**
 * Theme-aware CodeMirror styling — replaces the hardcoded `oneDark` import
 * that kept the editor permanently dark regardless of the active app theme
 * (docs/COMPLETION-AUDIT-2026-06-08.md, surfaces-b).
 *
 * Strategy:
 *   - All chrome colors (background, text, gutters, selection, cursor,
 *     panels, tooltips) are authored as `var(--token)` references against the
 *     custom properties the theme-engine writes onto `:root`. CSS variables
 *     resolve live, so when the user switches themes the chrome re-colors
 *     WITHOUT any reconfiguration — critical for keep-alive panes that stay
 *     mounted across theme changes.
 *   - Two things CodeMirror can't express through CSS variables — the
 *     `dark` flag on the theme (gates `&dark`/`&light` base-theme rules) and
 *     the syntax HighlightStyle (a fixed palette per mode) — are derived from
 *     the resolved mode and swapped via a Compartment on the theme-engine's
 *     THEME_CHANGED_EVENT (see EditorPane).
 *
 * Pure DOM reads + extension construction; no React, no Tauri.
 */
import { EditorView } from "@codemirror/view";
import type { Extension } from "@codemirror/state";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { oneDarkHighlightStyle } from "@codemirror/theme-one-dark";

/** Resolved app theme mode. The theme-engine mirrors the active theme's mode
 *  onto `:root[data-theme-mode]`; the global.css default palette is dark, so
 *  an absent attribute means dark. */
export function appThemeMode(): "dark" | "light" {
  if (typeof document === "undefined") return "dark";
  return document.documentElement.dataset.themeMode === "light" ? "light" : "dark";
}

/** Chrome theme — every color is a token reference, so it tracks the active
 *  theme live. Only the `dark` flag is baked at construction time. */
function chromeTheme(dark: boolean): Extension {
  return EditorView.theme(
    {
      "&": {
        backgroundColor: "var(--bg)",
        color: "var(--text)",
      },
      ".cm-content": { caretColor: "var(--accent)" },
      ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)" },
      "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
        { backgroundColor: "var(--accent-glow)" },
      ".cm-activeLine": { backgroundColor: "var(--bg-hover)" },
      ".cm-gutters": {
        backgroundColor: "var(--bg)",
        color: "var(--text-muted)",
        border: "none",
        borderRight: "1px solid var(--border)",
      },
      ".cm-activeLineGutter": {
        backgroundColor: "var(--bg-hover)",
        color: "var(--text)",
      },
      ".cm-selectionMatch": { backgroundColor: "var(--accent-soft)" },
      ".cm-searchMatch": {
        backgroundColor: "var(--accent-soft)",
        outline: "1px solid var(--accent-dim)",
      },
      ".cm-searchMatch.cm-searchMatch-selected": {
        backgroundColor: "var(--accent-glow)",
      },
      "&.cm-focused .cm-matchingBracket": {
        backgroundColor: "var(--accent-soft)",
        outline: "1px solid var(--accent-dim)",
      },
      "&.cm-focused .cm-nonmatchingBracket": { color: "var(--danger)" },
      ".cm-panels": {
        backgroundColor: "var(--bg-elev, var(--bg-elevated))",
        color: "var(--text)",
      },
      ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
      ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
      ".cm-panel input, .cm-panel select": {
        backgroundColor: "var(--input-bg, var(--bg-sunken))",
        color: "var(--text)",
        border: "1px solid var(--border)",
      },
      ".cm-tooltip": {
        backgroundColor: "var(--bg-elev, var(--bg-elevated))",
        color: "var(--text)",
        border: "1px solid var(--border)",
      },
      ".cm-tooltip-autocomplete ul li[aria-selected]": {
        backgroundColor: "var(--accent-soft)",
        color: "var(--text)",
      },
      ".cm-placeholder": { color: "var(--text-muted)" },
      ".cm-foldPlaceholder": {
        backgroundColor: "var(--bg-hover)",
        color: "var(--text-muted)",
        border: "1px solid var(--border)",
      },
    },
    { dark },
  );
}

/**
 * Full theme extension for the current app theme. Wrap in a Compartment and
 * reconfigure on THEME_CHANGED_EVENT so the `dark` flag + highlight palette
 * follow dark↔light switches; the var()-based chrome follows every token
 * change automatically.
 *
 * Syntax colors use the CodeMirror-curated palette per mode (oneDark's for
 * dark themes, the classic default for light themes) — the app token set has
 * no per-syntax-scope tokens, and a fixed, legible code palette per mode is
 * the same contract VS Code themes follow.
 */
export function cortexEditorTheme(): Extension {
  const dark = appThemeMode() === "dark";
  return [
    chromeTheme(dark),
    syntaxHighlighting(dark ? oneDarkHighlightStyle : defaultHighlightStyle, {
      fallback: true,
    }),
  ];
}
