// Bundle mobile/native/bridge.ts into mobile/dist/native-bridge.js and inject
// a classic <script> tag for it into mobile/dist/index.html, ahead of the
// SPA's module script (classic scripts run before deferred module scripts, so
// `window.CortexNative` exists when React boots).
//
// Run AFTER `pnpm build:mobile` and BEFORE `cap sync`:
//   cd mobile/native && npm install && npm run bridge
//
// esbuild is resolved from mobile/native/node_modules regardless of cwd.

import { createRequire } from "node:module";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");
const nativeDir = path.join(repoRoot, "mobile", "native");
const distDir = path.join(repoRoot, "mobile", "dist");
const indexHtml = path.join(distDir, "index.html");
const outFile = path.join(distDir, "native-bridge.js");

if (!existsSync(indexHtml)) {
  console.error(
    `build-bridge: ${indexHtml} not found — run \`pnpm build:mobile\` first`,
  );
  process.exit(1);
}

const require = createRequire(path.join(nativeDir, "package.json"));
const esbuild = require("esbuild");
const version = JSON.parse(
  readFileSync(path.join(repoRoot, "package.json"), "utf8"),
).version;

await esbuild.build({
  entryPoints: [path.join(nativeDir, "bridge.ts")],
  bundle: true,
  format: "iife",
  platform: "browser",
  target: ["es2020", "safari15", "chrome90"],
  minify: true,
  sourcemap: false,
  outfile: outFile,
  define: { __CORTEX_VERSION__: JSON.stringify(version) },
  logLevel: "info",
});

const TAG = '<script src="./native-bridge.js"></script>';
let html = readFileSync(indexHtml, "utf8");
if (!html.includes(TAG)) {
  if (html.includes("</head>")) {
    html = html.replace("</head>", `    ${TAG}\n  </head>`);
  } else {
    html = html.replace("<script", `${TAG}\n<script`);
  }
  writeFileSync(indexHtml, html);
  console.log(
    `build-bridge: injected ${TAG} into ${path.relative(repoRoot, indexHtml)}`,
  );
} else {
  console.log("build-bridge: index.html already references native-bridge.js");
}
