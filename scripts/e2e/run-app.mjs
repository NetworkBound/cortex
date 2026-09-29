#!/usr/bin/env node
// Launch the REAL built Cortex desktop app with the E2E probe armed and assert
// that the renderer actually came up.
//
// How it works (see src-tauri/src/commands/e2e.rs + src/lib/e2e-probe.ts):
// with `CORTEX_E2E=1` the renderer heartbeats a JSON snapshot of its own live
// state (DOM mounted, theme tokens applied, JS errors, feature-flow results) to
// the backend every 3 s, which writes it atomically to
// `$CORTEX_E2E_DIR/snapshot.json` (default `~/.cortex/e2e/snapshot.json`).
// That file can only exist if the web process is alive and running JS, so on
// WebKitGTK a black-screen build (web process aborted on EGL init) simply
// writes nothing — the absence of a fresh snapshot is the failure signal.
//
// Usage:
//   node scripts/e2e/run-app.mjs --bin src-tauri/target/release/cortex[.exe]
//        [--timeout 120] [--settle 20] [--out e2e-out] [--keep-home]
//        [--expect-version 3.2.0] [--allow-console-errors]
//        [--dist dist] [--baseline e2e-baseline/metrics.json]
//
// Speed: besides the pass/fail checks the runner prints a boot-metrics table
// (mount, first paint, projects loaded, IPC calls in the first 5 s, DOM nodes,
// JS heap, bundle size) from the snapshot's `perf` block + the backend's
// `timing` envelope, writes it to `<out>/metrics.json` + `<out>/metrics.md`
// (the workflow appends the .md to the job summary), and judges it against
// soft budgets: `E2E_BUDGET_*` env vars (see BUDGETS below) WARN by default and
// FAIL only with `E2E_STRICT=1`. When a previous run's metrics.json is given
// (`--baseline` / `E2E_BASELINE`), deltas are printed too (never fatal).
//
// Linux CI runs this under `xvfb-run -a` (see .github/workflows/e2e.yml).
// Exit code 0 = pass, 1 = assertion/launch failure, 2 = usage error.
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import {
  budgetFromEnv,
  fail,
  isStrict,
  isWindows,
  judge,
  killTree,
  launch,
  log,
  makeIsolatedHome,
  mdTable,
  parseArgs,
  removeDir,
  resolveBin,
  sleep,
  tailFile,
  textTable,
  warn,
} from "./lib.mjs";

const args = parseArgs(process.argv.slice(2));
const bin = resolveBin(args.bin ?? args._[0] ?? process.env.CORTEX_APP_BIN);
if (!bin || !fs.existsSync(bin)) {
  console.error(
    `usage: run-app.mjs --bin <path-to-cortex-binary>\n  (got: ${bin ?? "(none)"})`,
  );
  process.exit(2);
}
const timeoutSec = Number(args.timeout ?? 120);
// How long to keep collecting heartbeats after the first fresh snapshot, so
// early feature flows + late-boot errors have a chance to land.
const outDir = path.resolve(args.out ?? "e2e-out");
const expectVersion = args["expect-version"];
const allowConsoleErrors = Boolean(args["allow-console-errors"]);
// Built desktop frontend (vite `dist/`, what tauri build embeds) — read for
// the bundle-size metrics; skipped when absent.
const distDir = path.resolve(args.dist ?? "dist");
const baselinePath =
  args.baseline ??
  process.env.E2E_BASELINE ??
  path.join("e2e-baseline", "metrics.json");

// Soft budgets (ms / counts). Generous on purpose: CI runners are slow,
// GPU-less VMs; the point is to catch a 2× regression, not to tune. Override
// any of them with the env var named here; E2E_STRICT=1 makes them fatal.
const BUDGETS = {
  mountMs: budgetFromEnv("E2E_BUDGET_MOUNT_MS", 4000),
  fcpMs: budgetFromEnv("E2E_BUDGET_FCP_MS", 4000),
  projectsLoadedMs: budgetFromEnv("E2E_BUDGET_PROJECTS_MS", 6000),
  // Backend boot → main window visible. main.tsx reveals the window after
  // its first painted frame; lib.rs' watchdog forces it at 8 s, so anything
  // ≥ 8000 means the frontend never got there on its own.
  windowShownMs: budgetFromEnv("E2E_BUDGET_WINDOW_SHOWN_MS", 8000),
  firstSnapshotFromLaunchMs: budgetFromEnv(
    "E2E_BUDGET_FIRST_SNAPSHOT_MS",
    15000,
  ),
  invokeCountFirst5s: budgetFromEnv("E2E_BUDGET_INVOKES", 150),
  domNodes: budgetFromEnv("E2E_BUDGET_DOM_NODES", 8000),
  jsHeapUsedMB: budgetFromEnv("E2E_BUDGET_HEAP_MB", 256),
  bundleJsKB: budgetFromEnv("E2E_BUDGET_JS_KB", 8000),
};
// A metric that got worse than this (percent AND an absolute floor) against
// the baseline is called out. Informational only.
const REGRESSION_PCT = budgetFromEnv("E2E_REGRESSION_PCT", 25);
// The probe writes every POLL_MS (3000) — a snapshot older than this is stale,
// i.e. the web process wrote once and then died/hung.
const PROBE_POLL_MS = 3000;
const STALE_AFTER_MS = PROBE_POLL_MS * 3 + 2000;
// The settle window must outlast the stale threshold, otherwise a renderer
// that wrote once and died would still look "fresh" at assertion time.
const settleSec = Math.max(
  Number(args.settle ?? 20),
  Math.ceil((STALE_AFTER_MS + 3000) / 1000),
);

// Console errors that are EXPECTED in a clean profile with no gateway, no
// Ollama, no CLIs installed and no network: every one of these is the app
// reporting an unreachable optional dependency, not a bug. Anything else in
// `errors[]` (and every `uncaught` error, always) fails the run.
const ALLOWED_CONSOLE_ERRORS = [
  /ECONNREFUSED|connection refused|connect error|error sending request/i,
  /Failed to fetch|Load failed|NetworkError|fetch failed|network error/i,
  /ENOTFOUND|EAI_AGAIN|dns error|name resolution/i,
  /timed? ?out|deadline/i,
  /gateway (is )?(not configured|unreachable|offline|down)|no gateway|gateway_base_url/i,
  /ollama/i,
  /tailscale|tsnet/i,
  /keyring|secret service|credential|Platform secure storage/i,
  /not (found|installed|configured|available|reachable)|No such file|ENOENT|program not found|os error 2\b/i,
  /not a git repository|git (failed|not found)/i,
  /no (models|adapters?|endpoints?|project)/i,
  /apply_profile failed/i, // profiles need the gateway; expected offline
];

function isAllowedError(entry) {
  if (entry.kind === "uncaught") return false;
  return ALLOWED_CONSOLE_ERRORS.some((re) => re.test(entry.message ?? ""));
}

/** Sum the built frontend's JS/CSS chunks (vite `dist/assets`). */
function bundleStats(dir) {
  const assets = path.join(dir, "assets");
  if (!fs.existsSync(assets)) return null;
  const out = {
    jsChunks: 0,
    jsKB: 0,
    cssFiles: 0,
    cssKB: 0,
    largestJs: null,
    largestJsKB: 0,
  };
  for (const name of fs.readdirSync(assets)) {
    let size = 0;
    try {
      size = fs.statSync(path.join(assets, name)).size;
    } catch {
      continue;
    }
    const kb = size / 1024;
    if (name.endsWith(".js")) {
      out.jsChunks++;
      out.jsKB += kb;
      if (kb > out.largestJsKB) {
        out.largestJsKB = kb;
        out.largestJs = name;
      }
    } else if (name.endsWith(".css")) {
      out.cssFiles++;
      out.cssKB += kb;
    }
  }
  out.jsKB = Math.round(out.jsKB);
  out.cssKB = Math.round(out.cssKB);
  out.largestJsKB = Math.round(out.largestJsKB);
  return out;
}

const diff = (a, b) =>
  typeof a === "number" && typeof b === "number" ? a - b : null;

/**
 * Turn the latest envelope into the flat metrics record. Renderer marks are
 * relative to `performance.timeOrigin`; backend ones to `timing.boot_ms`;
 * `firstSnapshotFromLaunchMs` is what the runner itself observed (spawn →
 * first fresh snapshot, so it includes process start + webview init).
 */
function collectMetrics(first, latest, startedAt) {
  const snap = latest.env.snapshot ?? {};
  const p = snap.perf ?? {};
  const t = latest.env.timing ?? {};
  const bundle = bundleStats(distDir);
  const metrics = {
    // renderer (ms since navigation start)
    mountMs: p.mountMs ?? null,
    fcpMs: p.fcpMs ?? null,
    fcpSource: p.fcpSource ?? null,
    domContentLoadedMs: p.domContentLoadedMs ?? null,
    projectsLoadedMs: p.projectsLoadedMs ?? null,
    gatewayResolvedMs: p.gatewayResolvedMs ?? null,
    invokeCountFirst5s: p.invokeWindowClosed ? p.invokeCountFirst5s : null,
    invokeCountTotal: p.invokeCountTotal ?? null,
    domNodes: p.domNodes ?? snap.dom?.totalNodes ?? null,
    jsHeapUsedMB: p.jsHeapUsedMB ?? null,
    // backend (ms since boot_ms)
    rendererJsMs: diff(t.config_ms, t.boot_ms),
    windowShownMs: diff(t.window_shown_ms, t.boot_ms),
    firstSnapshotMs: diff(t.first_snapshot_ms, t.boot_ms),
    // runner-observed
    firstSnapshotFromLaunchMs: first.env.received_at - startedAt,
    // bundle
    bundleJsChunks: bundle?.jsChunks ?? null,
    bundleJsKB: bundle?.jsKB ?? null,
    bundleCssKB: bundle?.cssKB ?? null,
  };
  return { metrics, bundle, perf: p, timing: t };
}

function loadBaseline() {
  try {
    const raw = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
    if (raw && typeof raw.metrics === "object") {
      return { path: baselinePath, metrics: raw.metrics };
    }
  } catch {
    /* no baseline — fine */
  }
  return null;
}

/** Print + persist the metrics table; returns the budget verdicts. */
function reportMetrics(first, latest, startedAt) {
  const { metrics, bundle, perf, timing } = collectMetrics(
    first,
    latest,
    startedAt,
  );
  const verdicts = [
    judge("mount (first React commit)", metrics.mountMs, BUDGETS.mountMs, "ms"),
    judge(
      `first paint (${metrics.fcpSource ?? "n/a"})`,
      metrics.fcpMs,
      BUDGETS.fcpMs,
      "ms",
    ),
    judge(
      "projects loaded (list_projects)",
      metrics.projectsLoadedMs,
      BUDGETS.projectsLoadedMs,
      "ms",
    ),
    judge(
      "window shown (backend boot → visible)",
      metrics.windowShownMs,
      BUDGETS.windowShownMs,
      "ms",
    ),
    judge(
      "first snapshot (spawn → file)",
      metrics.firstSnapshotFromLaunchMs,
      BUDGETS.firstSnapshotFromLaunchMs,
      "ms",
    ),
    judge(
      "IPC invokes in first 5 s",
      metrics.invokeCountFirst5s,
      BUDGETS.invokeCountFirst5s,
    ),
    judge("DOM nodes", metrics.domNodes, BUDGETS.domNodes),
    judge("JS heap used", metrics.jsHeapUsedMB, BUDGETS.jsHeapUsedMB, "MB"),
    judge("bundle JS total", metrics.bundleJsKB, BUDGETS.bundleJsKB, "KB"),
  ];

  const baseline = loadBaseline();
  const deltaFor = (key) => {
    const cur = metrics[key];
    const prev = baseline?.metrics?.[key];
    if (typeof cur !== "number" || typeof prev !== "number") return "";
    const d = cur - prev;
    const pct = prev === 0 ? 0 : (d / prev) * 100;
    const sign = d > 0 ? "+" : "";
    return `${sign}${Math.round(d)} (${sign}${pct.toFixed(0)}%)`;
  };
  const verdictKeys = [
    "mountMs",
    "fcpMs",
    "projectsLoadedMs",
    "windowShownMs",
    "firstSnapshotFromLaunchMs",
    "invokeCountFirst5s",
    "domNodes",
    "jsHeapUsedMB",
    "bundleJsKB",
  ];
  const unitOf = (k) =>
    k.endsWith("Ms")
      ? "ms"
      : k.endsWith("MB")
        ? "MB"
        : k.endsWith("KB")
          ? "KB"
          : "";
  const rows = verdicts.map((v, i) => {
    const k = verdictKeys[i];
    return [
      v.name,
      v.value == null ? "n/a" : `${v.value}${unitOf(k)}`,
      `${v.budget}${unitOf(k)}`,
      v.status,
      baseline ? deltaFor(k) || "-" : "",
    ];
  });
  // Informational rows (no budget).
  const info = [
    ["gateway_status resolved", metrics.gatewayResolvedMs, "ms"],
    ["DOMContentLoaded", metrics.domContentLoadedMs, "ms"],
    ["renderer JS up (backend boot → e2e_config)", metrics.rendererJsMs, "ms"],
    ["IPC invokes total (at last heartbeat)", metrics.invokeCountTotal, ""],
    ["bundle JS chunks", metrics.bundleJsChunks, ""],
    ["bundle CSS total", metrics.bundleCssKB, "KB"],
    [
      "largest JS chunk",
      bundle?.largestJs ? `${bundle.largestJs} ${bundle.largestJsKB}KB` : null,
      "",
    ],
  ].map(([name, v, unit]) => [
    name,
    v == null ? "n/a" : `${v}${unit}`,
    "-",
    "info",
    "",
  ]);
  const header = ["metric", "value", "budget", "status"];
  if (baseline) header.push("Δ vs baseline");
  const allRows = [...rows, ...info].map((r) => (baseline ? r : r.slice(0, 4)));

  console.log("\n=== boot metrics ===");
  console.log(textTable(header, allRows));
  if (baseline) log(`baseline: ${baseline.path}`);
  console.log("====================\n");

  // Baseline regressions: percent AND an absolute floor so a 40 → 60 ms
  // jitter on a tiny metric doesn't shout.
  if (baseline) {
    for (const k of verdictKeys) {
      const cur = metrics[k];
      const prev = baseline.metrics[k];
      if (typeof cur !== "number" || typeof prev !== "number") continue;
      const d = cur - prev;
      const floor = unitOf(k) === "ms" ? 250 : 10;
      if (d > floor && prev > 0 && (d / prev) * 100 > REGRESSION_PCT) {
        warn(
          `${k} regressed vs baseline: ${prev} → ${cur} (+${((d / prev) * 100).toFixed(0)}%)`,
        );
      }
    }
  }

  const top = Array.isArray(perf.invokeTopCommands)
    ? perf.invokeTopCommands
    : [];
  const slow = Array.isArray(perf.invokeSlowestMs) ? perf.invokeSlowestMs : [];
  if (top.length) {
    console.log(
      "top IPC commands: " + top.map((c) => `${c.cmd}×${c.value}`).join(", "),
    );
  }
  if (slow.length) {
    console.log(
      "slowest IPC (max ms): " +
        slow.map((c) => `${c.cmd} ${c.value}ms`).join(", "),
    );
  }

  const record = {
    schema: 1,
    at: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    node: process.version,
    appVersion: latest.env.app_version ?? null,
    strict: isStrict,
    metrics,
    budgets: BUDGETS,
    verdicts,
    bundle,
    baseline: baseline
      ? { path: baseline.path, metrics: baseline.metrics }
      : null,
    invokeTopCommands: top,
    invokeSlowestMs: slow,
    timing,
  };
  fs.writeFileSync(
    path.join(outDir, "metrics.json"),
    JSON.stringify(record, null, 2),
  );
  const md = [
    `### Desktop app boot — ${process.platform}/${process.arch} (v${record.appVersion ?? "?"})`,
    "",
    mdTable(header, allRows),
    "",
    top.length
      ? `<details><summary>IPC detail</summary>\n\n${mdTable(
          ["command", "calls"],
          top.map((c) => [c.cmd, c.value]),
        )}\n\n${mdTable(
          ["command", "max ms"],
          slow.map((c) => [c.cmd, c.value]),
        )}\n\n</details>`
      : "",
    "",
  ].join("\n");
  fs.writeFileSync(path.join(outDir, "metrics.md"), md);
  return verdicts;
}

/** Still leave a metrics file behind when the app never produced a snapshot. */
function writeNoMetrics(reason) {
  const record = {
    schema: 1,
    at: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    status: "no-snapshot",
    reason,
    bundle: bundleStats(distDir),
  };
  fs.writeFileSync(
    path.join(outDir, "metrics.json"),
    JSON.stringify(record, null, 2),
  );
  fs.writeFileSync(
    path.join(outDir, "metrics.md"),
    `### Desktop app boot — ${process.platform}/${process.arch}\n\n**No snapshot produced** — ${reason}\n`,
  );
}

async function main() {
  fs.mkdirSync(outDir, { recursive: true });
  const { home, env: homeEnv } = makeIsolatedHome("cortex-e2e-app-");
  const e2eDir = path.join(home, "e2e");
  fs.mkdirSync(e2eDir, { recursive: true });

  const env = {
    ...process.env,
    ...homeEnv,
    CORTEX_E2E: "1",
    // Deterministic snapshot location on every OS (see lib.mjs on why HOME
    // alone isn't enough on Windows).
    CORTEX_E2E_DIR: e2eDir,
    // Never clash with a cortex-serve/desktop instance left over on 8788.
    CORTEX_MOBILE_PORT: "0",
    RUST_LOG: process.env.RUST_LOG ?? "info",
    RUST_BACKTRACE: process.env.RUST_BACKTRACE ?? "1",
  };
  if (!isWindows) {
    // Software rendering knobs for headless X (Xvfb) — WebKitGTK's GPU/DMABuf
    // paths abort the web process on GitHub's virtual GPU-less runners.
    env.WEBKIT_DISABLE_COMPOSITING_MODE ??= "1";
    env.WEBKIT_DISABLE_DMABUF_RENDERER ??= "1";
    env.LIBGL_ALWAYS_SOFTWARE ??= "1";
    env.GDK_BACKEND ??= "x11";
  }

  // Candidate snapshot paths: the env-redirected one (current e2e.rs) plus the
  // legacy `~/.cortex/e2e` under both the isolated and the real home (a binary
  // built before CORTEX_E2E_DIR existed, or Windows ignoring HOME).
  const candidates = [
    path.join(e2eDir, "snapshot.json"),
    path.join(home, ".cortex", "e2e", "snapshot.json"),
    path.join(os.homedir(), ".cortex", "e2e", "snapshot.json"),
  ];

  log(`binary   : ${bin}`);
  log(`temp home: ${home}`);
  log(`snapshot : ${candidates[0]}`);
  log(`display  : ${process.env.DISPLAY ?? "(none)"}`);

  const startedAt = Date.now();
  const app = launch("cortex", bin, [], { env, outDir, echo: false });
  const pid = app.child.pid;
  log(`launched pid ${pid}; waiting up to ${timeoutSec}s for a fresh snapshot`);

  const readSnapshot = () => {
    for (const p of candidates) {
      try {
        const raw = fs.readFileSync(p, "utf8");
        const env = JSON.parse(raw);
        // Fresh = written by THIS process after we started it. A stale file
        // from a previous run (same path, older pid) must never pass.
        if (env && env.pid === pid && env.received_at >= startedAt - 5000) {
          return { env, path: p };
        }
      } catch {
        /* missing / half-written / not ours */
      }
    }
    return null;
  };

  let first = null;
  const deadline = startedAt + timeoutSec * 1000;
  while (Date.now() < deadline) {
    if (app.exited) break;
    first = readSnapshot();
    if (first) break;
    await sleep(500);
  }

  if (app.exited) {
    const r = await app.exit;
    tailFile(app.logs.stderr);
    tailFile(app.logs.stdout);
    const reason = `app exited before producing a snapshot (code=${r.code} signal=${r.signal}${
      r.error ? ` error=${r.error.message}` : ""
    })`;
    fail(reason);
    writeNoMetrics(reason);
    return finish(app, home);
  }
  if (!first) {
    tailFile(app.logs.stderr);
    tailFile(app.logs.stdout);
    const reason =
      `no fresh snapshot after ${timeoutSec}s — the renderer never ran JS. ` +
      `On Linux this is the WebKitGTK black-screen failure mode (web process ` +
      `died before first paint); on Windows check WebView2 in the stderr log.`;
    fail(reason);
    writeNoMetrics(reason);
    return finish(app, home);
  }
  log(
    `first snapshot after ${((first.env.received_at - startedAt) / 1000).toFixed(1)}s (${first.path})`,
  );

  // Keep sampling heartbeats for the settle window so the runner sees the
  // steady state, not just the first paint.
  let latest = first;
  let beats = 1;
  const settleUntil = Date.now() + settleSec * 1000;
  while (Date.now() < settleUntil && !app.exited) {
    await sleep(1000);
    const s = readSnapshot();
    if (s && s.env.received_at !== latest.env.received_at) {
      latest = s;
      beats++;
    }
  }

  const snapPath = path.join(outDir, "snapshot.json");
  fs.writeFileSync(snapPath, JSON.stringify(latest.env, null, 2));
  console.log("\n=== snapshot (latest) ===");
  console.log(JSON.stringify(latest.env, null, 2));
  console.log("=== end snapshot ===\n");

  // ---------------------------------------------------------------- asserts
  const snap = latest.env.snapshot ?? {};
  const checks = [];
  const check = (name, ok, detail = "") => {
    checks.push({ name, ok, detail });
    log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
  };

  if (app.exited) {
    const r = await app.exit;
    check(
      "app still running",
      false,
      `exited code=${r.code} signal=${r.signal}`,
    );
  } else {
    check("app still running", true);
  }
  const age = Date.now() - latest.env.received_at;
  check(
    "heartbeat fresh",
    age < STALE_AFTER_MS,
    `last snapshot ${age}ms ago (${beats} heartbeats seen; stale after ${STALE_AFTER_MS}ms)`,
  );
  check(
    "app_version present",
    typeof latest.env.app_version === "string" &&
      latest.env.app_version.length > 0,
    `app_version=${latest.env.app_version}`,
  );
  if (expectVersion) {
    check(
      "app_version matches",
      latest.env.app_version === expectVersion,
      `${latest.env.app_version} vs expected ${expectVersion}`,
    );
  }
  check(
    "DOM mounted (#root has children)",
    snap.dom?.rootMounted === true,
    `rootChildren=${snap.dom?.rootChildren}`,
  );
  check(
    "app shell rendered (>= 30 DOM nodes)",
    (snap.dom?.totalNodes ?? 0) >= 30,
    `totalNodes=${snap.dom?.totalNodes}`,
  );
  check(
    "theme CSS variables applied (--bg/--accent)",
    snap.theme?.cssVarsApplied === true,
    `bg=${snap.theme?.cssBg} accent=${snap.theme?.cssAccent}`,
  );
  const painted = String(snap.theme?.paintedBodyBg ?? "");
  check(
    "body background painted (not transparent)",
    painted.length > 0 &&
      !/^rgba\(0,\s*0,\s*0,\s*0\)$/.test(painted) &&
      painted !== "transparent",
    `paintedBodyBg=${painted}`,
  );
  // null = no named theme active (default sheet) — nothing to compare.
  check(
    "active theme == painted theme",
    snap.theme?.themeMatches !== false,
    `themeMatches=${snap.theme?.themeMatches} active=${snap.theme?.activeName || "(default)"}`,
  );
  check(
    "url is the bundled app",
    typeof snap.url === "string" && !/localhost:1420/.test(snap.url),
    `url=${snap.url}`,
  );

  const errors = Array.isArray(snap.errors) ? snap.errors : [];
  const uncaught = errors.filter((e) => e.kind === "uncaught");
  const unexpected = errors.filter((e) => !isAllowedError(e));
  const allowed = errors.filter((e) => isAllowedError(e));
  if (allowed.length) {
    log(`${allowed.length} allow-listed error(s) (expected offline):`);
    for (const e of allowed) console.log(`   [${e.kind}] ${e.message}`);
  }
  if (unexpected.length) {
    log(`${unexpected.length} UNEXPECTED error(s):`);
    for (const e of unexpected) console.log(`   [${e.kind}] ${e.message}`);
  }
  check(
    "zero uncaught JS errors",
    uncaught.length === 0,
    `${uncaught.length} uncaught`,
  );
  const softUnexpected = unexpected.filter((e) => e.kind !== "uncaught");
  check(
    "no unexpected console.error / unhandled rejections",
    softUnexpected.length === 0 || allowConsoleErrors,
    `${softUnexpected.length} unexpected${allowConsoleErrors && softUnexpected.length ? " (tolerated via --allow-console-errors)" : ""}`,
  );

  // Feature flows are informational: most skip without Ollama/gateway. Print
  // them so a regression is visible in the log, but don't gate on them.
  const flows = snap.flows ?? {};
  console.log("\n=== feature flows ===");
  for (const [name, f] of Object.entries(flows)) {
    const state = !f?.attempted ? "pending" : f.settled ? "settled" : "running";
    console.log(`  ${name.padEnd(16)} ${state.padEnd(8)} ${f?.detail ?? ""}`);
  }
  console.log("=====================\n");

  // Speed: metrics table + soft budgets (fatal only under E2E_STRICT=1).
  const verdicts = reportMetrics(first, latest, startedAt);
  if (isStrict) {
    for (const v of verdicts) {
      if (v.ok === false)
        check(`budget: ${v.name}`, false, `${v.value} > ${v.budget}`);
    }
  }

  const failed = checks.filter((c) => !c.ok);
  if (failed.length) {
    tailFile(app.logs.stderr, 60);
    fail(
      `${failed.length} check(s) failed: ${failed.map((c) => c.name).join("; ")}`,
    );
  } else {
    log(`all ${checks.length} checks passed`);
  }
  return finish(app, home);
}

async function finish(app, home) {
  log("stopping app");
  const r = await killTree(app, 5000);
  log(`app stopped (code=${r?.code} signal=${r?.signal})`);
  // Keep the snapshot/logs (already in outDir); drop the temp home unless asked.
  if (args["keep-home"]) {
    log(`keeping temp home ${home}`);
  } else {
    removeDir(home);
  }
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
