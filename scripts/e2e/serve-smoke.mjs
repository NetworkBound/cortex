#!/usr/bin/env node
// Smoke-test the headless server binary (`cortex-serve`, see
// src-tauri/src/bin/cortex-serve.rs): start it with an isolated home on a free
// loopback port, then hit the real HTTP + WebSocket surface with Node's
// built-in `fetch` and a hand-rolled RFC 6455 handshake (so we can control the
// `Origin` header, which the browser-style WebSocket API deliberately hides).
//
// Checks:
//   - GET /api/health            → { ok: true, version }
//   - GET /                      → the mobile PWA index.html (SPA served)
//   - GET /<deep/link>           → index.html again (SPA fallback)
//   - GET /favicon.svg           → static asset from mobile/dist
//   - GET ./assets/<index>.js    → the hashed entry script referenced by index.html
//   - GET /v1/models             → OpenAI-style { object: "list", data: [...] }
//   - GET /api/sessions          → JSON array (fresh store → [])
//   - WS  /ws with no Origin     → 101 (native clients)
//   - WS  /ws Origin == Host     → 101 (same-origin SPA)
//   - WS  /ws Origin evil.example→ 403 (cross-site WebSocket hijack blocked)
//
// Usage:
//   node scripts/e2e/serve-smoke.mjs --bin src-tauri/target/release/cortex-serve[.exe]
//        [--dist mobile/dist] [--timeout 90] [--out e2e-out]
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import {
  fail,
  freePort,
  killTree,
  launch,
  log,
  makeIsolatedHome,
  parseArgs,
  removeDir,
  resolveBin,
  sleep,
  tailFile,
} from "./lib.mjs";

const args = parseArgs(process.argv.slice(2));
const bin = resolveBin(args.bin ?? args._[0] ?? process.env.CORTEX_SERVE_BIN);
if (!bin || !fs.existsSync(bin)) {
  console.error(
    `usage: serve-smoke.mjs --bin <path-to-cortex-serve> [--dist mobile/dist]\n  (got: ${bin ?? "(none)"})`,
  );
  process.exit(2);
}
const dist = path.resolve(args.dist ?? path.join("mobile", "dist"));
const timeoutSec = Number(args.timeout ?? 90);
const outDir = path.resolve(args.out ?? "e2e-out");
// Per-request budget. `/v1/models` probes every registered adapter (gateway,
// Ollama, CLIs) and each probe has its own connect timeout when offline.
const REQUEST_TIMEOUT_MS = 60_000;

const checks = [];
function check(name, ok, detail = "") {
  checks.push({ name, ok, detail });
  log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
  return ok;
}

async function get(base, p, init = {}) {
  const res = await fetch(new URL(p, base), {
    ...init,
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const text = await res.text();
  return { res, text, type: res.headers.get("content-type") ?? "" };
}

/**
 * Perform a raw WebSocket opening handshake against `/ws`. Resolves with
 * `{ status, accepted }` — `status` is the HTTP status line code (101 on
 * success) and `accepted` whether `Sec-WebSocket-Accept` matched. On 101 a
 * Close frame is sent before tearing the socket down.
 */
function wsHandshake(host, port, origin) {
  return new Promise((resolve, reject) => {
    const key = crypto.randomBytes(16).toString("base64");
    const expected = crypto
      .createHash("sha1")
      .update(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
      .digest("base64");
    const headers = {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": key,
    };
    if (origin) headers.Origin = origin;
    const req = http.request({
      host,
      port,
      path: "/ws",
      method: "GET",
      headers,
      timeout: 15_000,
    });
    req.on("upgrade", (res, socket) => {
      const accepted = res.headers["sec-websocket-accept"] === expected;
      // Unmasked-key Close frame (opcode 0x8, mask bit set, 4-byte mask, no payload).
      try {
        socket.write(Buffer.from([0x88, 0x80, 0, 0, 0, 0]));
      } catch {
        /* ignore */
      }
      socket.destroy();
      resolve({ status: res.statusCode, accepted });
    });
    req.on("response", (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () =>
        resolve({ status: res.statusCode, accepted: false, body }),
      );
    });
    req.on("timeout", () => {
      req.destroy(new Error("handshake timeout"));
    });
    req.on("error", reject);
    req.end();
  });
}

async function main() {
  fs.mkdirSync(outDir, { recursive: true });
  if (!fs.existsSync(path.join(dist, "index.html"))) {
    fail(`mobile dist not found at ${dist} (run \`pnpm build:mobile\` first)`);
    return;
  }
  const { home, env: homeEnv } = makeIsolatedHome("cortex-e2e-serve-");
  const port = await freePort();
  const host = "127.0.0.1";
  const base = `http://${host}:${port}/`;
  const env = {
    ...process.env,
    ...homeEnv,
    CORTEX_MOBILE_PORT: String(port),
    CORTEX_MOBILE_DIST: dist,
    RUST_LOG: process.env.RUST_LOG ?? "info",
    RUST_BACKTRACE: process.env.RUST_BACKTRACE ?? "1",
  };
  log(`binary   : ${bin}`);
  log(`dist     : ${dist}`);
  log(`temp home: ${home}`);
  log(`base url : ${base}`);

  const srv = launch("cortex-serve", bin, [], { env, outDir });

  // Wait for /api/health.
  const deadline = Date.now() + timeoutSec * 1000;
  let health = null;
  while (Date.now() < deadline && !srv.exited) {
    try {
      const r = await fetch(new URL("/api/health", base), {
        signal: AbortSignal.timeout(2000),
      });
      if (r.ok) {
        health = await r.json();
        break;
      }
    } catch {
      /* not up yet */
    }
    await sleep(500);
  }
  if (srv.exited) {
    const r = await srv.exit;
    tailFile(srv.logs.stderr);
    fail(
      `cortex-serve exited during startup (code=${r.code} signal=${r.signal})`,
    );
    return finish(srv, home);
  }
  if (!health) {
    tailFile(srv.logs.stderr);
    fail(`cortex-serve did not answer /api/health within ${timeoutSec}s`);
    return finish(srv, home);
  }
  check(
    "GET /api/health",
    health.ok === true && typeof health.version === "string",
    JSON.stringify(health),
  );

  try {
    // SPA index.
    const index = await get(base, "/");
    check(
      "GET / serves the mobile PWA index.html",
      index.res.status === 200 &&
        /text\/html/.test(index.type) &&
        /<div id="root">/.test(index.text) &&
        /<script[^>]+type="module"/.test(index.text),
      `status=${index.res.status} type=${index.type} bytes=${index.text.length}`,
    );

    // SPA fallback for deep links.
    const deep = await get(base, "/chat/some-session-id");
    check(
      "GET /<deep-link> falls back to index.html",
      deep.res.status === 200 &&
        /text\/html/.test(deep.type) &&
        /<div id="root">/.test(deep.text),
      `status=${deep.res.status} type=${deep.type}`,
    );

    // Static asset shipped in mobile/public.
    const fav = await get(base, "/favicon.svg");
    check(
      "GET /favicon.svg static asset",
      fav.res.status === 200 && /svg/.test(fav.type) && /<svg/i.test(fav.text),
      `status=${fav.res.status} type=${fav.type}`,
    );

    // The hashed entry script index.html references (base './' → "./assets/x.js").
    const m = index.text.match(/<script[^>]+src="([^"]+\.js)"/);
    if (m) {
      const rel = m[1].replace(/^\.\//, "/");
      const js = await get(base, rel);
      check(
        `GET ${rel} (entry script)`,
        js.res.status === 200 &&
          /javascript/.test(js.type) &&
          js.text.length > 1000,
        `status=${js.res.status} type=${js.type} bytes=${js.text.length}`,
      );
    } else {
      check(
        "index.html references an entry script",
        false,
        "no <script src> found",
      );
    }

    // OpenAI-compatible model list.
    const models = await get(base, "/v1/models");
    let modelsJson = null;
    try {
      modelsJson = JSON.parse(models.text);
    } catch {
      /* handled below */
    }
    check(
      "GET /v1/models",
      models.res.status === 200 &&
        modelsJson?.object === "list" &&
        Array.isArray(modelsJson?.data) &&
        modelsJson.data.every(
          (d) => typeof d.id === "string" && d.object === "model",
        ),
      `status=${models.res.status} models=${modelsJson?.data?.length ?? "?"}`,
    );

    // Sessions from the (fresh) tracing store.
    const sessions = await get(base, "/api/sessions");
    let sessionsJson = null;
    try {
      sessionsJson = JSON.parse(sessions.text);
    } catch {
      /* handled below */
    }
    check(
      "GET /api/sessions",
      sessions.res.status === 200 && Array.isArray(sessionsJson),
      `status=${sessions.res.status} body=${sessions.text.slice(0, 120)}`,
    );

    // CORS: a foreign origin must NOT be echoed back on /api/*.
    const cors = await get(base, "/api/health", {
      headers: { Origin: "https://evil.example" },
    });
    const acao = cors.res.headers.get("access-control-allow-origin");
    check(
      "CORS: foreign Origin not allowed on /api",
      cors.res.status === 200 &&
        acao !== "https://evil.example" &&
        acao !== "*",
      `access-control-allow-origin=${acao ?? "(none)"}`,
    );

    // WebSocket upgrade: no Origin (native client) → 101.
    const wsNative = await wsHandshake(host, port, undefined);
    check(
      "WS /ws upgrade without Origin",
      wsNative.status === 101 && wsNative.accepted,
      `status=${wsNative.status} accept-ok=${wsNative.accepted}`,
    );

    // Same-origin (Origin authority == Host) → 101.
    const wsSame = await wsHandshake(host, port, `http://${host}:${port}`);
    check(
      "WS /ws upgrade with same-origin Origin",
      wsSame.status === 101 && wsSame.accepted,
      `status=${wsSame.status} accept-ok=${wsSame.accepted}`,
    );

    // Foreign website → rejected (403), no upgrade.
    const wsEvil = await wsHandshake(host, port, "https://evil.example");
    check(
      "WS /ws upgrade from https://evil.example rejected",
      wsEvil.status === 403,
      `status=${wsEvil.status} body=${(wsEvil.body ?? "").slice(0, 80)}`,
    );

    // The server must still be alive after all of that.
    const again = await get(base, "/api/health");
    check("server still healthy", again.res.status === 200 && !srv.exited);
  } catch (e) {
    check("request sequence completed", false, e?.message ?? String(e));
  }

  const failed = checks.filter((c) => !c.ok);
  if (failed.length) {
    tailFile(srv.logs.stderr, 60);
    fail(
      `${failed.length} check(s) failed: ${failed.map((c) => c.name).join("; ")}`,
    );
  } else {
    log(`all ${checks.length} checks passed`);
  }
  return finish(srv, home);
}

async function finish(srv, home) {
  log("stopping cortex-serve");
  const r = await killTree(srv, 5000);
  log(`stopped (code=${r?.code} signal=${r?.signal})`);
  if (!args["keep-home"]) removeDir(home);
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
