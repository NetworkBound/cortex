/**
 * Custom theme loader + CSS-variable applier for Cortex.
 *
 * Themes are flat JSON documents (one per theme) loaded from
 * `~/.cortex/themes/<name>.json` by the Rust `list_themes` command. The
 * active-theme name + background image path live in `~/.cortex/themes.json`.
 *
 * "Applying" a theme just rewrites a handful of CSS custom properties on
 * `:root`. No rebuild, no stylesheet swap — paints are instant.
 *
 * Built-in presets are bundled at the bottom of this module so a fresh
 * install with zero user themes on disk still has something to pick. The
 * very first preset ("Zinc Amber") is intentionally a faithful copy of the
 * defaults baked into `global.css`, so applying it is a no-op.
 */
import { invoke } from "@tauri-apps/api/core";
import { applyCoreTokens, cacheCoreTokens } from "./theme-engine";
import { PALETTES, type ThemeId } from "./themes";

/**
 * Shape mirrors `CustomTheme` in `src-tauri/src/commands/themes.rs` for the
 * fields that round-trip to disk. The trailing block is **optional, in-memory
 * only** — the curated flagship/pro presets (promoted into the picker from
 * `themes.ts`) carry their hand-tuned, WCAG-audited border/hover/accent-fg/mode
 * verbatim instead of having the engine derive them, so they render in the
 * gallery at full curated fidelity. Disk themes (via `fromRust`) and
 * user-authored JSON omit these → the engine derives them exactly as before.
 */
export interface Theme {
  name: string;
  accent: string;
  accentStrong: string;
  accentDim: string;
  bg: string;
  bgElevated: string;
  bgSunken: string;
  text: string;
  textDim: string;
  textMuted: string;
  success: string;
  warning: string;
  danger: string;
  fontSans: string;
  fontMono: string;
  /** Full-fidelity curated tokens (written verbatim when present). */
  border?: string;
  borderStrong?: string;
  bgHover?: string;
  accentFg?: string;
  accentRgb?: string;
  info?: string;
  mode?: "dark" | "light";
}

export interface ActiveThemeState {
  active: string;
  bg_image_path: string | null;
}

/** Backend uses snake_case field aliases; we marshal both sides explicitly. */
interface RustTheme {
  name: string;
  accent: string;
  accent_strong: string;
  accent_dim: string;
  bg: string;
  bg_elevated: string;
  bg_sunken: string;
  text: string;
  text_dim: string;
  text_muted: string;
  success: string;
  warning: string;
  danger: string;
  font_sans: string;
  font_mono: string;
}

function fromRust(t: RustTheme): Theme {
  return {
    name: t.name,
    accent: t.accent,
    accentStrong: t.accent_strong,
    accentDim: t.accent_dim,
    bg: t.bg,
    bgElevated: t.bg_elevated,
    bgSunken: t.bg_sunken,
    text: t.text,
    textDim: t.text_dim,
    textMuted: t.text_muted,
    success: t.success,
    warning: t.warning,
    danger: t.danger,
    fontSans: t.font_sans,
    fontMono: t.font_mono,
  };
}

const DEFAULT_SANS =
  '"Inter", "SF Pro Display", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif';
const DEFAULT_MONO =
  '"JetBrains Mono", ui-monospace, "Cascadia Code", "SF Mono", Menlo, Consolas, monospace';

/**
 * Promote a curated palette from `themes.ts` into the picker's `Theme` shape,
 * carrying its full hand-tuned token set (border/hover/accent-fg/mode/…) so the
 * gallery preview and the applied result match the flagship exactly — no
 * derivation drift. The slug is the curated id (already a valid theme name);
 * the label comes from the curated metadata.
 */
function curated(id: ThemeId): Theme {
  const p = PALETTES[id];
  return {
    name: id,
    accent: p.accent,
    accentStrong: p.accentStrong,
    accentDim: p.accentDim,
    bg: p.bg,
    bgElevated: p.bgElevated,
    bgSunken: p.bgSunken,
    text: p.text,
    textDim: p.textDim,
    textMuted: p.textMuted,
    success: p.success,
    warning: p.warning,
    danger: p.danger,
    fontSans: DEFAULT_SANS,
    fontMono: DEFAULT_MONO,
    // Full-fidelity curated tokens — applied verbatim by the theme engine.
    border: p.border,
    borderStrong: p.borderStrong,
    bgHover: p.bgHover,
    accentFg: p.accentFg,
    accentRgb: p.accentRgb,
    info: p.info,
    mode: p.mode,
  };
}

/**
 * Built-in presets, ordered most-professional-first so the picker leads with
 * the flagship themes rather than burying them. The curated flagships
 * (Midnight/Daylight) and refined neutrals (Slate/Graphite/Paper) are promoted
 * from `themes.ts` at full fidelity; the warm default and the community
 * favourites follow. Names must match the Rust validator (letters/digits/`_-.`)
 * so users can save edits over the top.
 */
export const BUILTIN_THEMES: Theme[] = [
  // ── Flagship professional defaults ──────────────────────────────────────
  curated("midnight"), // Linear/GitHub-grade dark
  curated("daylight"), // Vercel/Raycast-grade light
  // ── Refined neutral pros ────────────────────────────────────────────────
  curated("slate"),
  curated("graphite"),
  {
    name: "carbon",
    accent: "#60a5fa",
    accentStrong: "#93c5fd",
    accentDim: "#2563eb",
    bg: "#0b1018",
    bgElevated: "#141b27",
    bgSunken: "#070a10",
    text: "#e6edf5",
    textDim: "#9aa6b8",
    textMuted: "#5d6878",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#f87171",
    fontSans: DEFAULT_SANS,
    fontMono: DEFAULT_MONO,
  },
  curated("paper"), // clean professional light
  // ── Warm default + expressive / community favourites ────────────────────
  {
    name: "zinc-amber",
    accent: "#fb923c",
    accentStrong: "#fdba74",
    accentDim: "#ea580c",
    bg: "#0a0a0c",
    bgElevated: "#121218",
    bgSunken: "#050507",
    text: "#ececf1",
    textDim: "#a8a8b3",
    textMuted: "#6e6e7a",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#f87171",
    fontSans: DEFAULT_SANS,
    fontMono: DEFAULT_MONO,
  },
  {
    name: "solarized-light",
    accent: "#b58900",
    accentStrong: "#cb9b1a",
    accentDim: "#8a6800",
    bg: "#fdf6e3",
    bgElevated: "#eee8d5",
    bgSunken: "#f5efdc",
    text: "#073642",
    textDim: "#586e75",
    textMuted: "#93a1a1",
    success: "#859900",
    warning: "#b58900",
    danger: "#dc322f",
    fontSans: DEFAULT_SANS,
    fontMono: DEFAULT_MONO,
  },
  {
    name: "tokyo-night",
    accent: "#7aa2f7", accentStrong: "#9eb8ff", accentDim: "#5d7bd6",
    bg: "#1a1b26", bgElevated: "#20212e", bgSunken: "#16161e",
    text: "#c0caf5", textDim: "#9aa5ce", textMuted: "#787c99",
    success: "#9ece6a", warning: "#e0af68", danger: "#f7768e",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
  {
    name: "dracula",
    accent: "#bd93f9", accentStrong: "#d0b3ff", accentDim: "#9d6ef0",
    bg: "#282a36", bgElevated: "#313442", bgSunken: "#21222c",
    text: "#f8f8f2", textDim: "#c3c3d1", textMuted: "#969aaf",
    success: "#50fa7b", warning: "#ffb86c", danger: "#ff5555",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
  {
    name: "nord",
    accent: "#88c0d0", accentStrong: "#8fbcbb", accentDim: "#5e81ac",
    bg: "#2e3440", bgElevated: "#3b4252", bgSunken: "#272c36",
    text: "#eceff4", textDim: "#d8dee9", textMuted: "#9aa4b8",
    success: "#a3be8c", warning: "#ebcb8b", danger: "#bf616a",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
  {
    name: "rose-pine",
    accent: "#c4a7e7", accentStrong: "#d7c4f0", accentDim: "#a781d8",
    bg: "#191724", bgElevated: "#1f1d2e", bgSunken: "#15131f",
    text: "#e0def4", textDim: "#b8b5d0", textMuted: "#908caa",
    success: "#5dc2a3", warning: "#f6c177", danger: "#eb6f92",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
  {
    name: "synthwave",
    accent: "#ff2e97", accentStrong: "#ff66b3", accentDim: "#d6177a",
    bg: "#1a1126", bgElevated: "#241634", bgSunken: "#130b1c",
    text: "#f5e9ff", textDim: "#c9a9e9", textMuted: "#9d7fc0",
    success: "#2ee6a6", warning: "#ffd166", danger: "#ff4d6d",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
  {
    name: "matrix",
    accent: "#22e36a", accentStrong: "#4dff8c", accentDim: "#16a34a",
    bg: "#050a07", bgElevated: "#0a160f", bgSunken: "#020604",
    text: "#c6f7d8", textDim: "#6ee7a8", textMuted: "#4f9e6f",
    success: "#22e36a", warning: "#d6e34a", danger: "#ff5c5c",
    fontSans: DEFAULT_SANS, fontMono: DEFAULT_MONO,
  },
];

export function isValidThemeName(name: string): boolean {
  return /^[A-Za-z0-9_.-]{1,64}$/.test(name);
}

/**
 * Apply a theme by writing CSS custom properties straight onto `:root`.
 *
 * Routed through the shared theme engine so a custom/preset theme gets the
 * FULL token set — the tokens it doesn't author are derived from the ones it
 * does: `--accent-fg` from the accent's luminance, `--accent-soft/-glow` from
 * the accent, `--border/-strong` and `--bg-hover` from bg×text, and the
 * elevation/shadow tier + `data-theme-mode` from the background (so a light
 * custom theme actually gets light shadows and the light-mode CSS overrides).
 *
 * Empty fields are still skipped, so a partial theme JSON falls back to
 * whatever `global.css` already had — partial overrides remain a feature.
 */
export function applyCustomTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const tokens = {
    accent: theme.accent,
    accentStrong: theme.accentStrong,
    accentDim: theme.accentDim,
    bg: theme.bg,
    bgElevated: theme.bgElevated,
    bgSunken: theme.bgSunken,
    text: theme.text,
    textDim: theme.textDim,
    textMuted: theme.textMuted,
    success: theme.success,
    warning: theme.warning,
    danger: theme.danger,
    fontSans: theme.fontSans,
    fontMono: theme.fontMono,
    // Optional curated tokens — written verbatim when a promoted flagship
    // supplies them; absent for disk/user JSON themes, which the engine
    // derives from bg×text×accent exactly as before.
    border: theme.border,
    borderStrong: theme.borderStrong,
    bgHover: theme.bgHover,
    accentFg: theme.accentFg,
    accentRgb: theme.accentRgb,
    info: theme.info,
    mode: theme.mode,
  };
  applyCoreTokens(root, tokens);
  // Funnel the exact tokens into the shared boot cache so the next launch
  // paints THIS theme on the first frame (no flash of the global.css default
  // or a divergent legacy palette). See theme-engine.ts cacheCoreTokens.
  cacheCoreTokens(tokens);
  root.dataset.customTheme = theme.name;
}

/**
 * Merge user-defined themes (from disk) on top of the built-ins. Custom
 * themes win when names collide so users can override a preset.
 */
export async function loadAllThemes(): Promise<Theme[]> {
  let custom: Theme[] = [];
  try {
    const raw = await invoke<RustTheme[]>("list_themes");
    custom = raw.map(fromRust);
  } catch {
    // Backend may be unavailable during early boot or in tests — fall back
    // to just the built-ins.
  }
  const byName = new Map<string, Theme>();
  for (const t of BUILTIN_THEMES) byName.set(t.name, t);
  for (const t of custom) byName.set(t.name, t);
  return Array.from(byName.values());
}

export async function getActiveThemeState(): Promise<ActiveThemeState> {
  try {
    return await invoke<ActiveThemeState>("get_active_theme");
  } catch {
    return { active: "", bg_image_path: null };
  }
}

export async function setActiveThemeName(name: string): Promise<ActiveThemeState> {
  return invoke<ActiveThemeState>("set_active_theme", { name });
}

/**
 * Look up a theme by name across built-ins and disk; falls back to the
 * first built-in if nothing matches.
 */
export async function resolveTheme(name: string): Promise<Theme> {
  const all = await loadAllThemes();
  return all.find((t) => t.name === name) ?? BUILTIN_THEMES[0];
}
