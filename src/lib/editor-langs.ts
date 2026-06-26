/**
 * Extension → CodeMirror language map.
 *
 * Each entry returns a thunk that lazily imports & instantiates the
 * matching language pack. Keeping it lazy lets bundlers tree-shake the
 * unused grammars when only a handful of file types are opened.
 */
import type { Extension } from "@codemirror/state";

type LangFactory = () => Promise<Extension>;

/** Lowercase extension (no leading dot) → factory. */
const EXT_TO_LANG: Record<string, LangFactory> = {
  // JavaScript / TypeScript / JSX / TSX
  js:    async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: false, typescript: false }),
  mjs:   async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: false, typescript: false }),
  cjs:   async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: false, typescript: false }),
  jsx:   async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: true,  typescript: false }),
  ts:    async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: false, typescript: true }),
  tsx:   async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: true,  typescript: true }),

  // Rust
  rs:    async () => (await import("@codemirror/lang-rust")).rust(),

  // Python
  py:    async () => (await import("@codemirror/lang-python")).python(),
  pyi:   async () => (await import("@codemirror/lang-python")).python(),

  // CSS-ish
  css:   async () => (await import("@codemirror/lang-css")).css(),
  scss:  async () => (await import("@codemirror/lang-css")).css(),
  sass:  async () => (await import("@codemirror/lang-css")).css(),
  less:  async () => (await import("@codemirror/lang-css")).css(),

  // HTML / XML-ish
  html:  async () => (await import("@codemirror/lang-html")).html(),
  htm:   async () => (await import("@codemirror/lang-html")).html(),
  xml:   async () => (await import("@codemirror/lang-html")).html(),
  svg:   async () => (await import("@codemirror/lang-html")).html(),

  // JSON
  json:  async () => (await import("@codemirror/lang-json")).json(),
  jsonc: async () => (await import("@codemirror/lang-json")).json(),

  // Markdown
  md:       async () => (await import("@codemirror/lang-markdown")).markdown(),
  markdown: async () => (await import("@codemirror/lang-markdown")).markdown(),
  mdx:      async () => (await import("@codemirror/lang-markdown")).markdown(),
};

/** Returns the lowercase extension of `path`, or `""` if it has none. */
export function extOf(path: string): string {
  const i = path.lastIndexOf(".");
  if (i < 0 || i === path.length - 1) return "";
  // strip everything before the final path separator, then take the ext
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (i < slash) return "";
  return path.slice(i + 1).toLowerCase();
}

/**
 * Resolve a language extension for the given path, or `null` when no
 * grammar matches — callers should fall back to a plain-text editor.
 */
export async function languageForPath(path: string): Promise<Extension | null> {
  const factory = EXT_TO_LANG[extOf(path)];
  if (!factory) return null;
  try {
    return await factory();
  } catch {
    return null;
  }
}

/** Human-readable language label for the status line. */
export function languageLabel(path: string): string {
  switch (extOf(path)) {
    case "js":
    case "mjs":
    case "cjs":  return "JavaScript";
    case "jsx":  return "JSX";
    case "ts":   return "TypeScript";
    case "tsx":  return "TSX";
    case "rs":   return "Rust";
    case "py":
    case "pyi":  return "Python";
    case "css":  return "CSS";
    case "scss": return "SCSS";
    case "sass": return "Sass";
    case "less": return "Less";
    case "html":
    case "htm":  return "HTML";
    case "xml":  return "XML";
    case "svg":  return "SVG";
    case "json":
    case "jsonc":return "JSON";
    case "md":
    case "markdown":
    case "mdx":  return "Markdown";
    default:     return "Plain text";
  }
}
