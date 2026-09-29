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
//   - POST /mcp                  → 404 (MCP server is off by default)
//   - /api/v2 (mobile contract)  → pair with the preset CORTEX_E2E_PAIR_CODE,
//                                  then /capabilities, /threads, /models with
//                                  the token; a bogus token and a missing one
//                                  get 401 (CORTEX_E2E_FORCE_AUTH=1 makes
//                                  loopback require auth too); a wrong pairing
//                                  code is 401; /devices lists + revokes.
//   - response times (warm)      → index.html, /v1/models, /api/sessions each
//                                  answer in < E2E_BUDGET_HTTP_MS (500 ms;
//                                  median of 3) — WARN only, FAIL with
//                                  E2E_STRICT=1. Written to
//                                  <out>/serve-metrics.{json,md} for the job
//                                  summary.
//
// Usage:
//   node scripts/e2e/serve-smoke.mjs --bin src-tauri/target/release/cortex-serve[.exe]
//        [--dist mobile/dist] [--timeout 90] [--out e2e-out]
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import {
  budgetFromEnv,
  fail,
  freePort,
  isStrict,
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
// Warm response-time budget per endpoint (median of TIMING_SAMPLES).
const HTTP_BUDGET_MS = budgetFromEnv("E2E_BUDGET_HTTP_MS", 500);
const TIMING_SAMPLES = 3;
// Preset pairing code the server accepts while CORTEX_E2E=1 (see
// mobile_server/pairing.rs). Six digits, like a real one.
const E2E_PAIR_CODE = process.env.CORTEX_E2E_PAIR_CODE ?? "424242";

const checks = [];
function check(name, ok, detail = "") {
  checks.push({ name, ok, detail });
  log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
  return ok;
}

async function get(base, p, init = {}) {
  const t0 = performance.now();
  const res = await fetch(new URL(p, base), {
    ...init,
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const text = await res.text();
  return {
    res,
    text,
    type: res.headers.get("content-type") ?? "",
    ms: performance.now() - t0,
  };
}

/** Median wall time (ms, headers + full body) of `n` sequential GETs. */
async function timeEndpoint(base, p, n = TIMING_SAMPLES) {
  const samples = [];
  for (let i = 0; i < n; i++) {
    const r = await get(base, p);
    samples.push({ ms: r.ms, status: r.res.status });
  }
  const sorted = samples.map((s) => s.ms).sort((a, b) => a - b);
  return {
    path: p,
    medianMs: Math.round(sorted[Math.floor(sorted.length / 2)]),
    minMs: Math.round(sorted[0]),
    maxMs: Math.round(sorted[sorted.length - 1]),
    status: samples[samples.length - 1].status,
  };
}

/**
 * Response-time checks. Runs AFTER the functional checks so every endpoint has
 * been hit at least once (adapters probed, index.html read from disk) — this
 * measures the warm path. Soft: WARN unless E2E_STRICT=1.
 */
async function reportTimings(base, health) {
  const targets = ["/", "/v1/models", "/api/sessions"];
  const results = [];
  for (const p of targets) {
    try {
      results.push(await timeEndpoint(base, p));
    } catch (e) {
      results.push({ path: p, error: e?.message ?? String(e) });
    }
  }
  const verdicts = results.map((r) =>
    r.error
      ? {
          name: `GET ${r.path}`,
          value: null,
          budget: HTTP_BUDGET_MS,
          ok: null,
          status: "error",
        }
      : judge(
          `GET ${r.path} (warm, median of ${TIMING_SAMPLES})`,
          r.medianMs,
          HTTP_BUDGET_MS,
          "ms",
        ),
  );
  const header = ["endpoint", "median", "min", "max", "budget", "status"];
  const rows = results.map((r, i) => [
    `GET ${r.path}`,
    r.error ? `error: ${r.error}` : `${r.medianMs}ms`,
    r.error ? "-" : `${r.minMs}ms`,
    r.error ? "-" : `${r.maxMs}ms`,
    `${HTTP_BUDGET_MS}ms`,
    verdicts[i].status,
  ]);
  console.log("\n=== cortex-serve response times ===");
  console.log(textTable(header, rows));
  console.log("===================================\n");

  const record = {
    schema: 1,
    at: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    serveVersion: health?.version ?? null,
    strict: isStrict,
    budgetMs: HTTP_BUDGET_MS,
    samples: TIMING_SAMPLES,
    endpoints: results,
    verdicts,
  };
  fs.writeFileSync(
    path.join(outDir, "serve-metrics.json"),
    JSON.stringify(record, null, 2),
  );
  fs.writeFileSync(
    path.join(outDir, "serve-metrics.md"),
    `### cortex-serve response times — ${process.platform}/${process.arch} (v${record.serveVersion ?? "?"})\n\n${mdTable(header, rows)}\n`,
  );
  return verdicts;
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
    // Mobile v2 contract checks: arm E2E so the preset pairing code is
    // accepted, and make loopback require a bearer so the gate is exercised.
    CORTEX_E2E: "1",
    CORTEX_E2E_PAIR_CODE: E2E_PAIR_CODE,
    CORTEX_E2E_FORCE_AUTH: "1",
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

    // MCP server endpoint is off by default (no ~/.cortex/mcp-server.json in
    // the isolated home) → 404, even with a bearer header.
    const mcp = await get(base, "/mcp", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer not-a-real-token",
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }),
    });
    check(
      "POST /mcp is 404 while disabled",
      mcp.res.status === 404,
      `status=${mcp.res.status} body=${mcp.text.slice(0, 80)}`,
    );

    // ── Mobile v2 contract ────────────────────────────────────────────
    await v2Checks(base);

    // The server must still be alive after all of that.
    const again = await get(base, "/api/health");
    check("server still healthy", again.res.status === 200 && !srv.exited);

    // Speed (warm path; every endpoint above has been hit once already).
    const verdicts = await reportTimings(base, health);
    if (isStrict) {
      for (const v of verdicts) {
        if (v.ok === false)
          check(`budget: ${v.name}`, false, `${v.value}ms > ${v.budget}ms`);
      }
    }
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

/** Parse JSON leniently; `null` when the body isn't JSON. */
function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

/**
 * Mobile v2 contract checks (see scratchpad mobile-contract.md). The server
 * was started with CORTEX_E2E_FORCE_AUTH=1, so even loopback must present a
 * paired device's bearer token — which lets us assert the 401 paths on
 * 127.0.0.1 where a "remote" peer can't be simulated.
 */
async function v2Checks(base) {
  // Unauthenticated → 401 with the JSON error envelope.
  const noAuth = await get(base, "/api/v2/capabilities");
  const noAuthJson = parseJson(noAuth.text);
  check(
    "GET /api/v2/capabilities without token is 401",
    noAuth.res.status === 401 && noAuthJson?.error?.code === "unauthorized",
    `status=${noAuth.res.status} body=${noAuth.text.slice(0, 100)}`,
  );

  // Bogus token → 401 too.
  const bogus = await get(base, "/api/v2/capabilities", {
    headers: { authorization: "Bearer not-a-paired-device" },
  });
  check(
    "GET /api/v2/capabilities with bogus token is 401",
    bogus.res.status === 401 &&
      parseJson(bogus.text)?.error?.code === "unauthorized",
    `status=${bogus.res.status}`,
  );

  // Wrong pairing code → 401.
  const badPair = await get(base, "/api/v2/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code: "000001", device_name: "e2e" }),
  });
  check(
    "POST /api/v2/pair with a wrong code is 401",
    badPair.res.status === 401,
    `status=${badPair.res.status} body=${badPair.text.slice(0, 100)}`,
  );

  // Pair with the preset code.
  const pair = await get(base, "/api/v2/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code: E2E_PAIR_CODE, device_name: "E2E phone" }),
  });
  const pairJson = parseJson(pair.text);
  const token = pairJson?.token;
  check(
    "POST /api/v2/pair with the preset code returns a token",
    pair.res.status === 200 &&
      typeof token === "string" &&
      token.length >= 32 &&
      typeof pairJson?.device_id === "string" &&
      typeof pairJson?.server_version === "string",
    `status=${pair.res.status} keys=${Object.keys(pairJson ?? {}).join(",")}`,
  );
  if (!token) return;
  const auth = { authorization: `Bearer ${token}` };

  const caps = await get(base, "/api/v2/capabilities", { headers: auth });
  const capsJson = parseJson(caps.text);
  check(
    "GET /api/v2/capabilities with token",
    caps.res.status === 200 &&
      typeof capsJson?.server_version === "string" &&
      Array.isArray(capsJson?.features) &&
      capsJson.features.includes("threads") &&
      Array.isArray(capsJson?.local_agents) &&
      typeof capsJson?.gateway === "boolean" &&
      capsJson?.device?.id === pairJson.device_id,
    `status=${caps.res.status} features=${capsJson?.features?.length ?? "?"}`,
  );

  const threads = await get(base, "/api/v2/threads", { headers: auth });
  const threadsJson = parseJson(threads.text);
  check(
    "GET /api/v2/threads with token",
    threads.res.status === 200 && Array.isArray(threadsJson?.threads),
    `status=${threads.res.status} body=${threads.text.slice(0, 100)}`,
  );

  // Create, rename, list, delete a thread (server-side persistence).
  const created = await get(base, "/api/v2/threads", {
    method: "POST",
    headers: { ...auth, "content-type": "application/json" },
    body: JSON.stringify({ title: "E2E thread" }),
  });
  const createdJson = parseJson(created.text);
  check(
    "POST /api/v2/threads creates a thread",
    created.res.status === 200 &&
      typeof createdJson?.id === "string" &&
      createdJson?.title === "E2E thread",
    `status=${created.res.status} body=${created.text.slice(0, 120)}`,
  );
  if (createdJson?.id) {
    const renamed = await get(base, `/api/v2/threads/${createdJson.id}`, {
      method: "PATCH",
      headers: { ...auth, "content-type": "application/json" },
      body: JSON.stringify({ title: "E2E renamed" }),
    });
    check(
      "PATCH /api/v2/threads/:id renames",
      renamed.res.status === 200 &&
        parseJson(renamed.text)?.title === "E2E renamed",
      `status=${renamed.res.status}`,
    );
    const listed = parseJson(
      (await get(base, "/api/v2/threads", { headers: auth })).text,
    );
    check(
      "GET /api/v2/threads lists the new thread",
      Array.isArray(listed?.threads) &&
        listed.threads.some((t) => t.id === createdJson.id),
    );
    const msgs = await get(base, `/api/v2/threads/${createdJson.id}/messages`, {
      headers: auth,
    });
    check(
      "GET /api/v2/threads/:id/messages (empty thread)",
      msgs.res.status === 200 &&
        Array.isArray(parseJson(msgs.text)?.messages) &&
        parseJson(msgs.text).messages.length === 0,
      `status=${msgs.res.status}`,
    );
    const deleted = await get(base, `/api/v2/threads/${createdJson.id}`, {
      method: "DELETE",
      headers: auth,
    });
    check(
      "DELETE /api/v2/threads/:id",
      deleted.res.status === 200,
      `status=${deleted.res.status}`,
    );
    const gone = await get(base, `/api/v2/threads/${createdJson.id}/messages`, {
      headers: auth,
    });
    check(
      "deleted thread is 404",
      gone.res.status === 404 &&
        parseJson(gone.text)?.error?.code === "not_found",
      `status=${gone.res.status}`,
    );
  }

  const models = await get(base, "/api/v2/models", { headers: auth });
  const modelsJson = parseJson(models.text);
  check(
    "GET /api/v2/models with token",
    models.res.status === 200 &&
      Array.isArray(modelsJson?.models) &&
      modelsJson.models.every(
        (m) =>
          typeof m.id === "string" &&
          typeof m.provider === "string" &&
          Array.isArray(m.capabilities) &&
          typeof m.local === "boolean" &&
          ["free", "low", "mid", "high"].includes(m.cost_tier),
      ),
    `status=${models.res.status} models=${modelsJson?.models?.length ?? "?"}`,
  );

  for (const p of [
    "/api/v2/runs",
    "/api/v2/approvals",
    "/api/v2/projects",
    "/api/v2/routines",
    "/api/v2/reliability?range=7d",
    "/api/v2/settings/mobile",
    "/api/v2/push/status",
  ]) {
    const r = await get(base, p, { headers: auth });
    check(
      `GET ${p} with token`,
      r.res.status === 200 && parseJson(r.text) !== null,
      `status=${r.res.status} body=${r.text.slice(0, 80)}`,
    );
  }

  // Devices: the paired phone is listed; revoking it kills the token.
  const devices = await get(base, "/api/v2/devices", { headers: auth });
  const devicesJson = parseJson(devices.text);
  check(
    "GET /api/v2/devices lists the paired device",
    devices.res.status === 200 &&
      Array.isArray(devicesJson?.devices) &&
      devicesJson.devices.some((d) => d.id === pairJson.device_id) &&
      devicesJson.devices.every((d) => !("token_sha256" in d)),
    `status=${devices.res.status}`,
  );
  const revoked = await get(base, `/api/v2/devices/${pairJson.device_id}`, {
    method: "DELETE",
    headers: auth,
  });
  const afterRevoke = await get(base, "/api/v2/capabilities", {
    headers: auth,
  });
  check(
    "DELETE /api/v2/devices/:id revokes the token",
    revoked.res.status === 200 && afterRevoke.res.status === 401,
    `revoke=${revoked.res.status} then capabilities=${afterRevoke.res.status}`,
  );
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
