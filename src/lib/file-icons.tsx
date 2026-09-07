// Extension → icon/color mapping for the FileExplorer rows (and any future
// file lists).
//
// This used to return full-color emoji glyphs (🟦 ts, 🦀 rs, 🐍 py, 🐳 docker,
// ☕ java, 🟥 scala, 🖼️ images…). That is the textbook "ransom-note file tree":
// a wild mix of colored squares, animals, and objects, each a different
// baseline, weight, and visual language, rendered by whatever emoji font the
// OS ships. Best-in-class editors (VSCode/Seti, Zed, Cursor) instead give
// every row ONE cohesive line-icon shape and convey the file *type* through a
// restrained per-language color tint. We do the same here: a Lucide icon per
// category (the same icon set the rest of the app's chrome uses, see
// activity-icons.tsx / vocab-icons.tsx) + the per-language brand color carried
// onto `currentColor`.

import type { LucideIcon } from "lucide-react";
import {
  Braces,
  Code,
  Coffee,
  Container,
  Database,
  File,
  FileArchive,
  FileCode,
  FileJson,
  FileText,
  Film,
  Folder,
  FolderOpen,
  Image as ImageIcon,
  KeyRound,
  Lock,
  Music,
  Palette,
  ScrollText,
  Settings,
  SquareTerminal,
  Table,
} from "lucide-react";

export interface FileIconSpec {
  Icon: LucideIcon;
  /** Optional brand tint applied via `color` (the icon uses currentColor). */
  color?: string;
}

const DIR_OPEN: FileIconSpec = { Icon: FolderOpen, color: "var(--accent)" };
const DIR_CLOSED: FileIconSpec = { Icon: Folder, color: "var(--accent-strong)" };
const DEFAULT_FILE: FileIconSpec = { Icon: File };

// Extension → icon + tint. Keys are lowercase, no leading dot. Shapes are
// chosen by *category* (code / data / docs / image / config / …) so siblings
// share a visual language; the color carries the per-language identity.
const EXT_MAP: Record<string, FileIconSpec> = {
  // TypeScript / JS family
  ts: { Icon: FileCode, color: "#3178c6" },
  tsx: { Icon: FileCode, color: "#3178c6" },
  js: { Icon: FileCode, color: "#f7df1e" },
  jsx: { Icon: FileCode, color: "#f7df1e" },
  mjs: { Icon: FileCode, color: "#f7df1e" },
  cjs: { Icon: FileCode, color: "#f7df1e" },
  // Systems langs
  rs: { Icon: FileCode, color: "#dea584" },
  go: { Icon: FileCode, color: "#00add8" },
  c: { Icon: FileCode, color: "#5d9cec" },
  h: { Icon: FileCode, color: "#5d9cec" },
  cpp: { Icon: FileCode, color: "#00599c" },
  hpp: { Icon: FileCode, color: "#00599c" },
  zig: { Icon: FileCode, color: "#f7a41d" },
  // Scripting
  py: { Icon: FileCode, color: "#3572a5" },
  rb: { Icon: FileCode, color: "#cc342d" },
  sh: { Icon: SquareTerminal, color: "#89e051" },
  bash: { Icon: SquareTerminal, color: "#89e051" },
  fish: { Icon: SquareTerminal, color: "#4aae47" },
  zsh: { Icon: SquareTerminal, color: "#89e051" },
  // Web
  html: { Icon: Code, color: "#e34c26" },
  htm: { Icon: Code, color: "#e34c26" },
  css: { Icon: Palette, color: "#563d7c" },
  scss: { Icon: Palette, color: "#c6538c" },
  sass: { Icon: Palette, color: "#c6538c" },
  less: { Icon: Palette, color: "#1d365d" },
  vue: { Icon: FileCode, color: "#42b883" },
  svelte: { Icon: FileCode, color: "#ff3e00" },
  // Data
  json: { Icon: FileJson, color: "#f1c40f" },
  jsonc: { Icon: FileJson, color: "#f1c40f" },
  yaml: { Icon: Braces, color: "#cb171e" },
  yml: { Icon: Braces, color: "#cb171e" },
  toml: { Icon: Braces, color: "#9c4221" },
  xml: { Icon: Braces, color: "#0060ac" },
  csv: { Icon: Table, color: "#2e7d32" },
  sql: { Icon: Database, color: "#e38c00" },
  // Docs
  md: { Icon: FileText, color: "#519aba" },
  mdx: { Icon: FileText, color: "#519aba" },
  rst: { Icon: FileText, color: "#519aba" },
  txt: { Icon: FileText },
  pdf: { Icon: FileText, color: "#e53935" },
  log: { Icon: ScrollText, color: "#888" },
  // Images
  png: { Icon: ImageIcon, color: "#a1887f" },
  jpg: { Icon: ImageIcon, color: "#a1887f" },
  jpeg: { Icon: ImageIcon, color: "#a1887f" },
  gif: { Icon: ImageIcon, color: "#a1887f" },
  webp: { Icon: ImageIcon, color: "#a1887f" },
  svg: { Icon: ImageIcon, color: "#ff9800" },
  ico: { Icon: ImageIcon, color: "#a1887f" },
  // Config / build
  env: { Icon: KeyRound, color: "#fbc02d" },
  lock: { Icon: Lock, color: "#888" },
  dockerfile: { Icon: Container, color: "#2496ed" },
  gitignore: { Icon: Settings, color: "#f05033" },
  gitattributes: { Icon: Settings, color: "#f05033" },
  // Archives
  zip: { Icon: FileArchive, color: "#888" },
  tar: { Icon: FileArchive, color: "#888" },
  gz: { Icon: FileArchive, color: "#888" },
  // Audio / video
  mp3: { Icon: Music, color: "#9c27b0" },
  mp4: { Icon: Film, color: "#9c27b0" },
  // JVM
  java: { Icon: Coffee, color: "#b07219" },
  kt: { Icon: FileCode, color: "#a97bff" },
  scala: { Icon: FileCode, color: "#c22d40" },
};

/**
 * Resolve the lowercased extension from a filename. Returns "" when the name
 * has no dot or is purely a dotfile (e.g. `.env` → "env", `.gitignore` →
 * "gitignore"). We treat the last dot as the separator so `foo.tar.gz` →
 * "gz", which matches editor convention.
 */
export function extOf(name: string): string {
  if (!name) return "";
  // Dotfiles (`.env`, `.gitignore`) — the leading dot IS the separator.
  if (name.startsWith(".") && name.indexOf(".", 1) === -1) {
    return name.slice(1).toLowerCase();
  }
  const i = name.lastIndexOf(".");
  if (i <= 0) return "";
  return name.slice(i + 1).toLowerCase();
}

/** Look up the icon spec for a file by name. Directories use `dirIconFor`. */
export function fileIconFor(name: string): FileIconSpec {
  // Special-case `Dockerfile` (no extension).
  if (name.toLowerCase() === "dockerfile") return EXT_MAP.dockerfile;
  return EXT_MAP[extOf(name)] ?? DEFAULT_FILE;
}

export function dirIconFor(open: boolean): FileIconSpec {
  return open ? DIR_OPEN : DIR_CLOSED;
}

/**
 * Cohesive file-tree icon. Renders the per-category Lucide line-icon, tinted
 * with the language brand color (falls back to the inherited row color when a
 * type has no tint). Sized to sit in the 18px glyph gutter.
 */
export function FileIcon({
  name,
  size = 14,
}: {
  name: string;
  size?: number;
}) {
  const { Icon, color } = fileIconFor(name);
  return (
    <Icon
      size={size}
      strokeWidth={1.75}
      color="currentColor"
      style={color ? { color } : undefined}
      aria-hidden
    />
  );
}

/** Directory icon (open/closed), tinted with the accent. */
export function DirIcon({ open, size = 14 }: { open: boolean; size?: number }) {
  const { Icon, color } = dirIconFor(open);
  return (
    <Icon
      size={size}
      strokeWidth={1.75}
      color="currentColor"
      style={color ? { color } : undefined}
      aria-hidden
    />
  );
}
