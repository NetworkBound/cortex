/**
 * Runtime theme switcher for Cortex.
 *
 * Each theme is a COMPLETE palette: applying it writes the full set of core
 * CSS custom properties (`--bg`, `--accent`, `--text*`, `--border*`, …) onto
 * `document.documentElement`, so light themes flip every token and never fall
 * back to dark defaults left over in `global.css`. The `data-theme` attribute
 * is also set (handy for attribute-scoped CSS and debugging).
 *
 * Persistence: localStorage["cortex.theme"].
 */

import { applyCoreTokens, cacheCoreTokens } from "./theme-engine";

const STORAGE_KEY = "cortex.theme";

export type ThemeId =
  | "coral-dark"
  | "midnight"
  | "daylight"
  | "cyber-red"
  | "bone-light"
  | "solar-amber"
  | "slate"
  | "paper"
  | "graphite"
  | "tokyo-night"
  | "dracula"
  | "nord"
  | "rose-pine"
  | "synthwave"
  | "matrix";

export const THEMES: { id: ThemeId; label: string; description: string }[] = [
  {
    id: "coral-dark",
    label: "Coral Dark",
    description: "Claude-inspired dark UI with a warm coral accent (default).",
  },
  {
    id: "midnight",
    label: "Midnight",
    description:
      "Flagship dark — cool near-black surfaces, restrained indigo accent. Linear-grade.",
  },
  {
    id: "daylight",
    label: "Daylight",
    description:
      "Flagship light — crisp white, clean blue accent, true AA contrast. Vercel/Raycast-grade.",
  },
  {
    id: "slate",
    label: "Slate",
    description: "Refined cool-gray dark with a soft indigo accent.",
  },
  {
    id: "graphite",
    label: "Graphite",
    description: "Neutral charcoal dark with a restrained teal accent.",
  },
  {
    id: "solar-amber",
    label: "Solar Amber",
    description: "Warm dark surfaces with an amber/gold accent.",
  },
  {
    id: "cyber-red",
    label: "Cyber Red",
    description: "Near-black surfaces with a bright neon-red accent.",
  },
  {
    id: "paper",
    label: "Paper",
    description: "Clean professional light — off-white paper, dark ink text.",
  },
  {
    id: "bone-light",
    label: "Bone Light",
    description: "Cream paper background, charcoal text, coral accent.",
  },
  {
    id: "tokyo-night",
    label: "Tokyo Night",
    description: "Deep navy surfaces with a calm blue accent — the beloved dev theme.",
  },
  {
    id: "dracula",
    label: "Dracula",
    description: "Classic charcoal-violet dark with a soft purple accent.",
  },
  {
    id: "nord",
    label: "Nord",
    description: "Arctic slate-blue surfaces with a frost-cyan accent.",
  },
  {
    id: "rose-pine",
    label: "Rosé Pine",
    description: "Muted, elegant dark with a soft iris accent.",
  },
  {
    id: "synthwave",
    label: "Synthwave",
    description: "Futuristic neon — hot magenta + cyan on deep purple-black.",
  },
  {
    id: "matrix",
    label: "Matrix",
    description: "Futuristic phosphor-green terminal on near-black.",
  },
];

const THEME_IDS: ThemeId[] = THEMES.map((t) => t.id);

/**
 * Full per-theme palette. Every theme defines the complete token set the
 * default `:root` block in `global.css` defines, so switching to a light
 * theme genuinely flips bg/border/text and reads correctly. Values are tuned
 * for WCAG-ish contrast: body text >= ~7:1, dim text >= ~4.5:1 on its bg.
 */
export interface Palette {
  bg: string;
  bgElevated: string;
  bgSunken: string;
  bgHover: string;
  border: string;
  borderStrong: string;
  text: string;
  textDim: string;
  textMuted: string;
  accent: string;
  accentStrong: string;
  accentDim: string;
  /** Foreground placed ON the accent (buttons). */
  accentFg: string;
  /** rgba triplet used for soft/glow/selection tints, e.g. "251, 146, 60". */
  accentRgb: string;
  success: string;
  warning: string;
  danger: string;
  info: string;
  /** "dark" tunes shadows/elevation; "light" softens them. */
  mode: "dark" | "light";
}

export const PALETTES: Record<ThemeId, Palette> = {
  "coral-dark": {
    bg: "#0a0a0c",
    bgElevated: "#121218",
    bgSunken: "#050507",
    bgHover: "#1a1a22",
    border: "#26262e",
    borderStrong: "#3a3a48",
    text: "#ececf1",
    textDim: "#a8a8b3",
    textMuted: "#85858f",
    accent: "#fb923c",
    accentStrong: "#fdba74",
    accentDim: "#ea580c",
    accentFg: "#1a0d00",
    accentRgb: "251, 146, 60",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#f87171",
    info: "#60a5fa",
    mode: "dark",
  },

  midnight: {
    // Flagship dark. A cool, near-black surface family with a faint blue cast
    // (Linear/GitHub-grade), a restrained indigo-violet accent that never
    // shouts, and GitHub's battle-tested semantic colors. Every text/bg pair
    // clears WCAG AA (body >=15:1, dim/muted >=4.9:1, accent-fg on accent
    // >=5.9:1). The most "professional default" of the dark themes.
    bg: "#0b0d11",
    bgElevated: "#13161d",
    bgSunken: "#07080b",
    bgHover: "#1a1e27",
    border: "#212630",
    borderStrong: "#333a47",
    text: "#eef1f6",
    textDim: "#9aa3b2",
    textMuted: "#80899a",
    accent: "#7c83ec",
    accentStrong: "#9aa0f2",
    accentDim: "#5b62d6",
    accentFg: "#0a0b14",
    accentRgb: "124, 131, 236",
    success: "#3fb950",
    warning: "#d29922",
    danger: "#f85149",
    info: "#58a6ff",
    mode: "dark",
  },

  daylight: {
    // Flagship light. Crisp near-white paper with soft neutral surfaces, a
    // clean professional blue accent, and deep semantic colors that stay AA on
    // white (Vercel/Raycast-grade). The reference for "light mode done right":
    // hairline borders that are visible but quiet, dim/muted text >=5:1.
    bg: "#fcfcfd",
    bgElevated: "#ffffff",
    bgSunken: "#f4f4f6",
    bgHover: "#eeeef1",
    border: "#e6e6ea",
    borderStrong: "#d3d3da",
    text: "#17171a",
    textDim: "#52525b",
    textMuted: "#6c6c75",
    accent: "#2563eb",
    accentStrong: "#1d4ed8",
    accentDim: "#3b82f6",
    accentFg: "#ffffff",
    accentRgb: "37, 99, 235",
    success: "#15803d",
    warning: "#b45309",
    danger: "#dc2626",
    info: "#2563eb",
    mode: "light",
  },

  slate: {
    // Cool, refined neutral dark with a soft indigo accent. Backgrounds carry
    // a faint blue cast; the accent is calm rather than electric.
    bg: "#0e1117",
    bgElevated: "#161b24",
    bgSunken: "#0a0d12",
    bgHover: "#1e242f",
    border: "#262d39",
    borderStrong: "#3a4456",
    text: "#e9edf4",
    textDim: "#a3adbd",
    textMuted: "#7a8696",
    accent: "#818cf8",
    accentStrong: "#a5b0fb",
    accentDim: "#6366f1",
    accentFg: "#0b0e18",
    accentRgb: "129, 140, 248",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#f87171",
    info: "#60a5fa",
    mode: "dark",
  },

  graphite: {
    // Pure neutral charcoal — no color cast in the grays — with a restrained
    // teal accent. Reads as a calm, tooling-grade dark theme.
    bg: "#0c0c0d",
    bgElevated: "#161617",
    bgSunken: "#070708",
    bgHover: "#1e1e20",
    border: "#2a2a2c",
    borderStrong: "#3d3d40",
    text: "#eaeaec",
    textDim: "#a6a6ab",
    textMuted: "#828289",
    accent: "#2dd4bf",
    accentStrong: "#5eead4",
    accentDim: "#14b8a6",
    accentFg: "#04130f",
    accentRgb: "45, 212, 191",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#f87171",
    info: "#60a5fa",
    mode: "dark",
  },

  "solar-amber": {
    bg: "#15110a",
    bgElevated: "#1f1810",
    bgSunken: "#0e0b06",
    bgHover: "#2a2114",
    border: "#352a1a",
    borderStrong: "#4a3a24",
    text: "#f3ead9",
    textDim: "#c2b393",
    textMuted: "#9a8a6b",
    accent: "#f59e0b",
    accentStrong: "#fbbf24",
    accentDim: "#d97706",
    accentFg: "#1a1000",
    accentRgb: "245, 158, 11",
    success: "#4ade80",
    warning: "#facc15",
    danger: "#f87171",
    info: "#60a5fa",
    mode: "dark",
  },

  "cyber-red": {
    bg: "#060608",
    bgElevated: "#101015",
    bgSunken: "#020203",
    bgHover: "#181820",
    border: "#241c20",
    borderStrong: "#3a2a30",
    text: "#f1ecee",
    textDim: "#b0a4a8",
    textMuted: "#897e82",
    accent: "#ef4444",
    accentStrong: "#f87171",
    accentDim: "#dc2626",
    accentFg: "#1a0303",
    accentRgb: "239, 68, 68",
    success: "#34d399",
    warning: "#fbbf24",
    danger: "#fb7185",
    info: "#60a5fa",
    mode: "dark",
  },

  paper: {
    // Clean professional light. Off-white paper, near-black ink, a restrained
    // indigo accent. Every token flips so light mode is genuinely readable:
    // borders are visible-but-soft, dim/muted text keep AA contrast on paper.
    bg: "#f7f7f5",
    bgElevated: "#ffffff",
    bgSunken: "#eeeeec",
    bgHover: "#e9e9e6",
    border: "#dededa",
    borderStrong: "#c4c4be",
    text: "#1c1c1e",
    textDim: "#52525b",
    textMuted: "#6e6e76",
    accent: "#4f46e5",
    accentStrong: "#4338ca",
    accentDim: "#6366f1",
    accentFg: "#ffffff",
    accentRgb: "79, 70, 229",
    success: "#15803d",
    warning: "#b45309",
    danger: "#dc2626",
    info: "#2563eb",
    mode: "light",
  },

  "bone-light": {
    bg: "#f4efe6",
    bgElevated: "#fbf8f1",
    bgSunken: "#ebe4d6",
    bgHover: "#e7dfce",
    border: "#ddd3c0",
    borderStrong: "#c4b89f",
    text: "#2b2620",
    textDim: "#5c554a",
    textMuted: "#70695c",
    accent: "#c2410c",
    accentStrong: "#9a3412",
    accentDim: "#ea580c",
    accentFg: "#fff7ed",
    accentRgb: "194, 65, 12",
    success: "#15803d",
    warning: "#b45309",
    danger: "#b91c1c",
    info: "#2563eb",
    mode: "light",
  },

  "tokyo-night": {
    // The popular Tokyo Night dev palette: deep navy surfaces with a faint
    // blue cast and a calm periwinkle accent.
    bg: "#1a1b26",
    bgElevated: "#20212e",
    bgSunken: "#16161e",
    bgHover: "#292e42",
    border: "#2a2e3f",
    borderStrong: "#3b4261",
    text: "#c0caf5",
    textDim: "#9aa5ce",
    textMuted: "#787c99",
    accent: "#7aa2f7",
    accentStrong: "#9eb8ff",
    accentDim: "#5d7bd6",
    accentFg: "#0c1020",
    accentRgb: "122, 162, 247",
    success: "#9ece6a",
    warning: "#e0af68",
    danger: "#f7768e",
    info: "#7dcfff",
    mode: "dark",
  },

  dracula: {
    // Dracula: charcoal-violet surfaces, soft purple accent, its signature
    // bright semantic set.
    bg: "#282a36",
    bgElevated: "#313442",
    bgSunken: "#21222c",
    bgHover: "#3a3d4d",
    border: "#3a3d4d",
    borderStrong: "#4d5066",
    text: "#f8f8f2",
    textDim: "#c3c3d1",
    textMuted: "#969aaf",
    accent: "#bd93f9",
    accentStrong: "#d0b3ff",
    accentDim: "#9d6ef0",
    accentFg: "#1a1024",
    accentRgb: "189, 147, 249",
    success: "#50fa7b",
    warning: "#ffb86c",
    danger: "#ff5555",
    info: "#8be9fd",
    mode: "dark",
  },

  nord: {
    // Nord: arctic slate-blue surfaces (Polar Night) with a frost-cyan accent
    // and the Aurora semantic colors.
    bg: "#2e3440",
    bgElevated: "#3b4252",
    bgSunken: "#272c36",
    bgHover: "#434c5e",
    border: "#434c5e",
    borderStrong: "#4c566a",
    text: "#eceff4",
    textDim: "#d8dee9",
    textMuted: "#9aa4b8",
    accent: "#88c0d0",
    accentStrong: "#8fbcbb",
    accentDim: "#5e81ac",
    accentFg: "#1a2028",
    accentRgb: "136, 192, 208",
    success: "#a3be8c",
    warning: "#ebcb8b",
    danger: "#bf616a",
    info: "#81a1c1",
    mode: "dark",
  },

  "rose-pine": {
    // Rosé Pine: muted, low-saturation dark with a soft iris accent — elegant
    // and easy on the eyes.
    bg: "#191724",
    bgElevated: "#1f1d2e",
    bgSunken: "#15131f",
    bgHover: "#26233a",
    border: "#26233a",
    borderStrong: "#403d52",
    text: "#e0def4",
    textDim: "#b8b5d0",
    textMuted: "#908caa",
    accent: "#c4a7e7",
    accentStrong: "#d7c4f0",
    accentDim: "#a781d8",
    accentFg: "#1a1426",
    accentRgb: "196, 167, 231",
    success: "#5dc2a3",
    warning: "#f6c177",
    danger: "#eb6f92",
    info: "#9ccfd8",
    mode: "dark",
  },

  synthwave: {
    // Futuristic neon: deep purple-black surfaces with a hot-magenta accent
    // and an electric-cyan info — an 80s synthwave grid look.
    bg: "#1a1126",
    bgElevated: "#241634",
    bgSunken: "#130b1c",
    bgHover: "#2e1d44",
    border: "#3a2356",
    borderStrong: "#4f2f73",
    text: "#f5e9ff",
    textDim: "#c9a9e9",
    textMuted: "#9d7fc0",
    accent: "#ff2e97",
    accentStrong: "#ff66b3",
    accentDim: "#d6177a",
    accentFg: "#1a0011",
    accentRgb: "255, 46, 151",
    success: "#2ee6a6",
    warning: "#ffd166",
    danger: "#ff4d6d",
    info: "#29e0ff",
    mode: "dark",
  },

  matrix: {
    // Futuristic phosphor terminal: near-black green-tinted surfaces with a
    // bright phosphor-green accent.
    bg: "#050a07",
    bgElevated: "#0a160f",
    bgSunken: "#020604",
    bgHover: "#102117",
    border: "#16301f",
    borderStrong: "#1f4a2e",
    text: "#c6f7d8",
    textDim: "#6ee7a8",
    textMuted: "#4f9e6f",
    accent: "#22e36a",
    accentStrong: "#4dff8c",
    accentDim: "#16a34a",
    accentFg: "#021006",
    accentRgb: "34, 227, 106",
    success: "#22e36a",
    warning: "#d6e34a",
    danger: "#ff5c5c",
    info: "#4dd0ff",
    mode: "dark",
  },
};

function isThemeId(value: unknown): value is ThemeId {
  return typeof value === "string" && THEME_IDS.includes(value as ThemeId);
}

/**
 * Apply a theme by writing the complete core token set onto the document root.
 * Inline custom properties win over `global.css` defaults, so this guarantees
 * a full, hole-free palette regardless of which theme is active.
 */
export function applyTheme(id: ThemeId): void {
  if (typeof document !== "undefined") {
    const root = document.documentElement;
    const p = PALETTES[id] ?? PALETTES["coral-dark"];

    // Curated palettes supply every field (incl. border/hover/accentFg/mode),
    // so the shared engine writes them verbatim — no derivation — and also
    // sets data-theme-mode + colorScheme + the matching shadow tier.
    const tokens = {
      bg: p.bg,
      bgElevated: p.bgElevated,
      bgSunken: p.bgSunken,
      bgHover: p.bgHover,
      border: p.border,
      borderStrong: p.borderStrong,
      text: p.text,
      textDim: p.textDim,
      textMuted: p.textMuted,
      accent: p.accent,
      accentStrong: p.accentStrong,
      accentDim: p.accentDim,
      accentFg: p.accentFg,
      accentRgb: p.accentRgb,
      success: p.success,
      warning: p.warning,
      danger: p.danger,
      info: p.info,
      mode: p.mode,
    };
    applyCoreTokens(root, tokens);
    // Funnel the exact tokens into the shared boot cache so the next launch
    // paints THIS theme on the first frame (no flash of a divergent palette).
    cacheCoreTokens(tokens);

    root.dataset.theme = id;
  }
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, id);
    }
  } catch {
    // localStorage may be unavailable (private mode, SSR) — ignore
  }
}

export function loadTheme(): ThemeId {
  try {
    if (typeof localStorage !== "undefined") {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (isThemeId(raw)) return raw;
    }
  } catch {
    // ignore
  }
  return "coral-dark";
}

export function cycleTheme(): ThemeId {
  const current = loadTheme();
  const idx = THEME_IDS.indexOf(current);
  const next = THEME_IDS[(idx + 1) % THEME_IDS.length];
  applyTheme(next);
  return next;
}

// NOTE: first paint is no longer driven from here. This module used to run
// `applyTheme(loadTheme())` at import time, but that re-applied the LEGACY
// `localStorage["cortex.theme"]` palette — a different store from the canonical
// backend theme the picker writes — so the two drifted and every launch flashed
// the wrong theme before `useThemeBoot` corrected it. Boot now replays the
// shared token cache (`applyCachedCoreTokens`, see main.tsx) which BOTH systems
// feed via `cacheCoreTokens`, so the first frame already shows the last theme
// the user actually applied, regardless of which UI set it.
