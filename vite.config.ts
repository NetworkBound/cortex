import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

const host = process.env.TAURI_DEV_HOST;

// Vendor libraries that get their own long-lived chunk. Everything here is
// either heavy and only reached from a lazily-loaded surface (CodeMirror,
// xterm, mermaid, the markdown/highlight stack) or shared by the whole app
// and rarely changing (react, lucide, tauri) — splitting it keeps the startup
// bundle down to app code and lets the webview cache the vendor chunks across
// releases that only touch app code. Matching is on the package directory
// under node_modules, so scoped packages match on their scope.
const VENDOR_CHUNKS: Array<[chunk: string, packages: string[]]> = [
  [
    "vendor-codemirror",
    [
      "@codemirror",
      "codemirror",
      "@lezer",
      "style-mod",
      "w3c-keyname",
      "crelt",
    ],
  ],
  ["vendor-xterm", ["@xterm"]],
  ["vendor-mermaid", ["mermaid"]],
  ["vendor-highlight", ["highlight.js", "lowlight", "rehype-highlight"]],
  [
    "vendor-markdown",
    [
      "react-markdown",
      "remark-gfm",
      "remark-parse",
      "remark-rehype",
      "rehype-",
      "remark-",
      "mdast-",
      "micromark",
      "hast-",
      "unist-",
      "unified",
      "vfile",
      "bail",
      "trough",
      "devlop",
      "zwitch",
      "ccount",
      "longest-streak",
      "markdown-table",
      "trim-lines",
      "property-information",
      "space-separated-tokens",
      "comma-separated-tokens",
      "html-url-attributes",
      "estree-util-is-identifier-name",
      "decode-named-character-reference",
      "character-entities",
      "style-to-js",
      "style-to-object",
      "inline-style-parser",
    ],
  ],
  ["vendor-lucide", ["lucide-react"]],
  ["vendor-react", ["react", "react-dom", "scheduler"]],
  ["vendor-tauri", ["@tauri-apps"]],
];

/** Package directory name (`@scope/name` → `@scope`, `name@1.2.3` in pnpm
 *  stores → `name`) for a module id under node_modules, or null. */
function vendorPackage(id: string): string | null {
  const marker = "node_modules/";
  const at = id.lastIndexOf(marker);
  if (at < 0) return null;
  const rest = id.slice(at + marker.length);
  const first = rest.split("/")[0];
  // pnpm virtual-store entries look like `.pnpm/react@18.3.1/node_modules/react`
  // — `lastIndexOf` above already landed us on the real package dir, so
  // `first` is the bare name (or scope) here.
  return first.startsWith(".") ? null : first;
}

function manualChunks(id: string): string | undefined {
  const pkg = vendorPackage(id);
  if (!pkg) return undefined;
  for (const [chunk, packages] of VENDOR_CHUNKS) {
    for (const p of packages) {
      if (p.endsWith("-") ? pkg.startsWith(p) : pkg === p) return chunk;
    }
  }
  return undefined;
}

export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  clearScreen: false,
  build: {
    // WebView2 (Chromium, evergreen) and WebKitGTK ≥ 2.40 both implement
    // ES2022 natively, so ship modern syntax instead of down-levelling class
    // fields / optional chaining / top-level await into slower shims.
    target: "es2022",
    rollupOptions: {
      output: { manualChunks },
    },
    // The mermaid chunk alone is well over the default 500 kB advisory; it is
    // only fetched when the Architecture tab renders a diagram.
    chunkSizeWarningLimit: 1_500,
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
}));
