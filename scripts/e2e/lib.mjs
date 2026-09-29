// Shared helpers for the E2E runners (`run-app.mjs`, `serve-smoke.mjs`).
// Plain Node 20+, no dependencies. Works on Linux, macOS and Windows.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

export const isWindows = process.platform === "win32";

/** Minimal `--flag value` / `--flag` / `--flag=value` parser. */
export function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) {
      out._.push(a);
      continue;
    }
    const eq = a.indexOf("=");
    if (eq !== -1) {
      out[a.slice(2, eq)] = a.slice(eq + 1);
      continue;
    }
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next !== undefined && !next.startsWith("--")) {
      out[key] = next;
      i++;
    } else {
      out[key] = true;
    }
  }
  return out;
}

export function log(...args) {
  const ts = new Date().toISOString().slice(11, 23);
  console.log(`[e2e ${ts}]`, ...args);
}

export function fail(reason) {
  console.error(`\n[e2e] FAIL: ${reason}`);
  process.exitCode = 1;
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Non-fatal finding: printed prominently but never changes the exit code. */
export function warn(reason) {
  console.log(`[e2e] WARN: ${reason}`);
}

/**
 * Soft performance budgets. `E2E_STRICT=1` turns a blown budget into a
 * failure; otherwise it's a warning so a slow runner can't break the build
 * while a real regression still shows up in the log and the job summary.
 */
export const isStrict = /^(1|true|yes|on)$/i.test(process.env.E2E_STRICT ?? "");

/** Read a numeric budget from `process.env[name]`, falling back to `def`. */
export function budgetFromEnv(name, def) {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return def;
  const n = Number(raw);
  return Number.isFinite(n) ? n : def;
}

/**
 * Check a measured value against a budget. Returns a verdict row for the
 * metrics table; a blown budget is a WARN, or a FAIL under E2E_STRICT (the
 * caller then records it as a failed check so it lands in the final tally).
 * `value == null` means "not measured on this platform" and is neither.
 */
export function judge(name, value, budget, unit = "") {
  if (value == null) {
    return { name, value, budget, ok: null, status: "n/a" };
  }
  const ok = value <= budget;
  if (!ok && !isStrict) {
    warn(`${name} = ${value}${unit} exceeds budget ${budget}${unit}`);
  }
  return {
    name,
    value,
    budget,
    ok,
    status: ok ? "ok" : isStrict ? "FAIL" : "WARN",
  };
}

/** Render rows (arrays of cells) as a GitHub-flavoured Markdown table. */
export function mdTable(header, rows) {
  const esc = (c) => String(c ?? "").replace(/\|/g, "\\|");
  const line = (cells) => `| ${cells.map(esc).join(" | ")} |`;
  return [
    line(header),
    `|${header.map(() => " --- ").join("|")}|`,
    ...rows.map(line),
  ].join("\n");
}

/** Fixed-width console rendering of the same rows. */
export function textTable(header, rows) {
  const all = [header, ...rows].map((r) => r.map((c) => String(c ?? "")));
  const widths = header.map((_, i) =>
    Math.max(...all.map((r) => (r[i] ?? "").length)),
  );
  const fmtRow = (r) => r.map((c, i) => c.padEnd(widths[i])).join("  ");
  return [fmtRow(all[0]), widths.map((w) => "-".repeat(w)).join("  ")]
    .concat(all.slice(1).map(fmtRow))
    .join("\n");
}

/**
 * Create an isolated, throwaway "home" and return the env vars that steer
 * Cortex (and the platform dirs crates) into it.
 *
 * Linux/macOS: `dirs::home_dir()` reads `$HOME`, `data_local_dir()` reads
 * `$XDG_DATA_HOME`, so the app's `~/.cortex/*` state and its SQLite store both
 * land under the temp dir.
 *
 * Windows: the `dirs` crate resolves the profile through the known-folder API
 * (SHGetKnownFolderPath) and IGNORES `HOME`/`USERPROFILE`/`LOCALAPPDATA`. There
 * the app still writes `~/.cortex` + `%LOCALAPPDATA%\cortex` into the real
 * profile of the runner user; only the E2E bridge output is redirected via
 * `CORTEX_E2E_DIR` (honoured by `src-tauri/src/commands/e2e.rs`). A hosted CI
 * runner is a fresh machine, so that is acceptable. `CORTEX_TEST_HOME` is set
 * too for completeness but is only honoured by `cfg(test)` builds.
 */
export function makeIsolatedHome(prefix = "cortex-e2e-") {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const xdg = (name) => {
    const p = path.join(home, name);
    fs.mkdirSync(p, { recursive: true });
    return p;
  };
  const env = {
    HOME: home,
    USERPROFILE: home,
    CORTEX_TEST_HOME: home,
    XDG_CONFIG_HOME: xdg(".config"),
    XDG_DATA_HOME: xdg(path.join(".local", "share")),
    XDG_CACHE_HOME: xdg(".cache"),
    XDG_STATE_HOME: xdg(path.join(".local", "state")),
  };
  return { home, env };
}

/** Ask the OS for a free TCP port on loopback. */
export function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

/**
 * Spawn `bin` with stdout/stderr teed into `<outDir>/<name>.{stdout,stderr}.log`
 * (and mirrored to our own stderr when `echo` is set). On POSIX the child is
 * started in its own process group so the whole tree can be killed later.
 */
export function launch(name, bin, args, { env, outDir, echo = false, cwd }) {
  fs.mkdirSync(outDir, { recursive: true });
  const outLog = fs.createWriteStream(path.join(outDir, `${name}.stdout.log`));
  const errLog = fs.createWriteStream(path.join(outDir, `${name}.stderr.log`));
  const child = spawn(bin, args, {
    env,
    cwd,
    stdio: ["ignore", "pipe", "pipe"],
    detached: !isWindows,
  });
  child.stdout.on("data", (d) => {
    outLog.write(d);
    if (echo) process.stderr.write(d);
  });
  child.stderr.on("data", (d) => {
    errLog.write(d);
    if (echo) process.stderr.write(d);
  });
  const exit = new Promise((resolve) => {
    child.on("exit", (code, signal) => resolve({ code, signal }));
    child.on("error", (e) => resolve({ code: null, signal: null, error: e }));
  });
  let exited = false;
  exit.then(() => {
    exited = true;
    outLog.end();
    errLog.end();
  });
  return {
    child,
    exit,
    get exited() {
      return exited;
    },
    logs: {
      stdout: path.join(outDir, `${name}.stdout.log`),
      stderr: path.join(outDir, `${name}.stderr.log`),
    },
  };
}

/**
 * Kill a launched process tree: `taskkill /T /F` on Windows, SIGTERM to the
 * process group then SIGKILL after `graceMs` elsewhere. Resolves once the
 * child has exited (or after a hard cap).
 */
export async function killTree(proc, graceMs = 5000) {
  const { child, exit } = proc;
  if (proc.exited || child.pid === undefined) return exit;
  if (isWindows) {
    spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
    });
  } else {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {
      try {
        child.kill("SIGTERM");
      } catch {
        /* already gone */
      }
    }
  }
  const timer = sleep(graceMs).then(() => "timeout");
  const first = await Promise.race([exit, timer]);
  if (first === "timeout") {
    if (isWindows) {
      spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], {
        stdio: "ignore",
      });
    } else {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        try {
          child.kill("SIGKILL");
        } catch {
          /* already gone */
        }
      }
    }
    await Promise.race([exit, sleep(5000)]);
  }
  return exit;
}

/** Print the last `n` lines of a log file (if it exists). */
export function tailFile(file, n = 40) {
  try {
    const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
    const tail = lines.slice(Math.max(0, lines.length - n)).join("\n");
    if (tail.trim()) {
      console.log(`--- tail ${file} ---\n${tail}\n--- end ---`);
    }
  } catch {
    /* no log */
  }
}

/** Resolve a binary path, appending `.exe` on Windows when needed. */
export function resolveBin(p) {
  if (!p) return p;
  if (fs.existsSync(p)) return path.resolve(p);
  if (
    isWindows &&
    !p.toLowerCase().endsWith(".exe") &&
    fs.existsSync(`${p}.exe`)
  )
    return path.resolve(`${p}.exe`);
  return path.resolve(p);
}

/** Best-effort recursive removal of the temp home. */
export function removeDir(dir) {
  try {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  } catch {
    /* leave it for the OS temp cleaner */
  }
}
