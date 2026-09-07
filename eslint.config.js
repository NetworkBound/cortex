// ESLint v9 flat config. Restores `pnpm lint` (the old .eslintrc was missing,
// so linting silently no-op'd under ESLint 9). Uses the plugins already in
// devDependencies; `globals` isn't installed so browser globals are inlined.
import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

const browserGlobals = {
  window: "readonly",
  document: "readonly",
  navigator: "readonly",
  console: "readonly",
  localStorage: "readonly",
  sessionStorage: "readonly",
  fetch: "readonly",
  setTimeout: "readonly",
  clearTimeout: "readonly",
  setInterval: "readonly",
  clearInterval: "readonly",
  requestAnimationFrame: "readonly",
  cancelAnimationFrame: "readonly",
  queueMicrotask: "readonly",
  structuredClone: "readonly",
  performance: "readonly",
  crypto: "readonly",
  AbortController: "readonly",
  WebSocket: "readonly",
  HTMLElement: "readonly",
  HTMLInputElement: "readonly",
  HTMLTextAreaElement: "readonly",
  HTMLDivElement: "readonly",
  Element: "readonly",
  Event: "readonly",
  KeyboardEvent: "readonly",
  MouseEvent: "readonly",
  CustomEvent: "readonly",
  ResizeObserver: "readonly",
  IntersectionObserver: "readonly",
  MutationObserver: "readonly",
  URL: "readonly",
  URLSearchParams: "readonly",
  Blob: "readonly",
  FileReader: "readonly",
  TextEncoder: "readonly",
  TextDecoder: "readonly",
  getComputedStyle: "readonly",
  matchMedia: "readonly",
};

export default [
  { ignores: ["dist/**", "src-tauri/**", "node_modules/**"] },
  js.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaVersion: "latest",
        sourceType: "module",
        ecmaFeatures: { jsx: true },
      },
      globals: browserGlobals,
    },
    plugins: {
      "@typescript-eslint": tsPlugin,
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...tsPlugin.configs.recommended.rules,
      ...reactHooks.configs.recommended.rules,
      // TS handles undefined-variable detection with full type info; the core
      // rule produces false positives on type-only references.
      "no-undef": "off",
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "warn",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/no-explicit-any": "off",
      "react-refresh/only-export-components": "off",
      // Style-level, not bugs: harmless redundant regex escapes (`[\/]`) the
      // author kept for readability. Surface as warnings, don't fail the build.
      "no-useless-escape": "warn",
      // exhaustive-deps fires on the in-component-function-used-in-effect
      // pattern even when the effect correctly re-binds on the state it reads.
      // Keep it visible (warn) but reserve errors for genuine hook violations.
      "react-hooks/exhaustive-deps": "warn",
      "react-hooks/rules-of-hooks": "error",
    },
  },
];
