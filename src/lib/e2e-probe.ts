// Linux-native E2E probe (renderer side). Pairs with `commands/e2e.rs` and
// `scripts/e2e-linux.mjs`.
//
// When the app is launched with `CORTEX_E2E=1`, this collects a snapshot of the
// renderer's *own* live state — proof the web process is alive and painting,
// plus the signals a headless runner can't see from outside (theme applied,
// DOM mounted, gateway reachable, console errors) — and hands it to the backend
// to persist at `~/.cortex/e2e/snapshot.json` every few seconds.
//
// On WebKitGTK (the Linux webview) none of this code can run unless the web
// process survived EGL init and is rendering. So a black-screen build writes
// no snapshot at all — the runner treats a missing/stale snapshot as a hard
// failure. When the snapshot *is* fresh, its fields let the runner assert on
// finer things (did the theme apply? is the gateway connected? any JS errors?).
//
// Production overhead is zero: without CORTEX_E2E the probe never arms.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect } from "react";
import { getActiveThemeState, resolveTheme } from "./themes-custom";
import { openProjectByPath } from "./open-project";
import type { ProjectMeta } from "./projects";
import { useCortexStore } from "../state/store";
import { startCookbookPull, startEvalRun, useJobs } from "../state/jobs";
import { getNotificationsSnapshot } from "./notification-center";

const POLL_MS = 3000;
const MAX_ERRORS = 25;

// Module-level ring buffer of JS errors, installed once. We capture both
// uncaught errors and `console.error` calls because a black/partial render
// often shows up as a thrown React error or a failed IPC call rather than a
// hard crash.
const errorLog: Array<{ at: number; kind: string; message: string }> = [];
let errorHooksInstalled = false;
// Retained so teardown can detach listeners and restore the original
// `console.error` (otherwise the override leaks across the app lifetime).
let errorHandler: ((e: ErrorEvent) => void) | undefined;
let rejectionHandler: ((e: PromiseRejectionEvent) => void) | undefined;
let origConsoleError: typeof console.error | undefined;
let patchedConsoleError: typeof console.error | undefined;

function installErrorHooks(): void {
  if (errorHooksInstalled) return;
  errorHooksInstalled = true;

  errorHandler = (e) => {
    pushError("uncaught", e.message || String(e.error ?? "unknown error"));
  };
  rejectionHandler = (e) => {
    pushError("unhandledrejection", String(e.reason ?? "unknown"));
  };
  window.addEventListener("error", errorHandler);
  window.addEventListener("unhandledrejection", rejectionHandler);

  origConsoleError = console.error.bind(console);
  patchedConsoleError = (...args: unknown[]) => {
    try {
      pushError("console.error", args.map((a) => safeStringify(a)).join(" "));
    } catch {
      /* never let instrumentation break the app */
    }
    origConsoleError?.(...args);
  };
  console.error = patchedConsoleError;
}

function uninstallErrorHooks(): void {
  if (!errorHooksInstalled) return;
  errorHooksInstalled = false;

  if (errorHandler) window.removeEventListener("error", errorHandler);
  if (rejectionHandler) window.removeEventListener("unhandledrejection", rejectionHandler);
  errorHandler = undefined;
  rejectionHandler = undefined;

  // Only restore if nobody re-wrapped `console.error` after us; otherwise leave
  // the later override intact rather than clobbering it.
  if (origConsoleError && console.error === patchedConsoleError) {
    console.error = origConsoleError;
  }
  origConsoleError = undefined;
  patchedConsoleError = undefined;
}

function pushError(kind: string, message: string): void {
  errorLog.push({ at: Date.now(), kind, message: message.slice(0, 500) });
  if (errorLog.length > MAX_ERRORS) errorLog.splice(0, errorLog.length - MAX_ERRORS);
}

function safeStringify(v: unknown): string {
  if (typeof v === "string") return v;
  if (v instanceof Error) return v.message;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// Live flow exercise: once per armed session, drive the REAL
// Cookbook-pull → `models:changed` chain end-to-end — invoke the real
// `cookbook_pull_model` on a model that's already installed (a cheap manifest
// re-check, no big download; it's literally the Cookbook button's own code
// path) and record whether the real backend event came back. The runner
// asserts on this: a successful pull whose event never arrives is a regression
// in the pull→picker-refresh hand-off. Skips (Ollama down, nothing installed,
// registry offline) are recorded honestly so the runner can warn instead of
// fail.
const FLOW_EVENT_WAIT_MS = 20_000;
const modelsChangedFlow = {
  attempted: false,
  settled: false,
  pullOk: null as boolean | null,
  eventSeen: false,
  detail: "not attempted",
};

async function exerciseModelsChangedFlow(): Promise<void> {
  if (modelsChangedFlow.attempted) return;
  modelsChangedFlow.attempted = true;
  try {
    const view = await invoke<{
      specs: { ollama_running: boolean };
      recommendations: Array<{ name: string; installed: boolean }>;
    }>("cookbook_recommendations");
    const installed = view.recommendations.find((r) => r.installed);
    if (!view.specs.ollama_running || !installed) {
      modelsChangedFlow.detail = !view.specs.ollama_running
        ? "skipped: ollama not running"
        : "skipped: no installed model to re-pull";
      return;
    }
    // Subscribe BEFORE pulling so the event can't race past us.
    let sawEvent: () => void = () => {};
    const eventArrived = new Promise<boolean>((resolve) => {
      sawEvent = () => resolve(true);
      setTimeout(() => resolve(false), FLOW_EVENT_WAIT_MS);
    });
    const unlisten = await listen("models:changed", () => sawEvent());
    try {
      const result = await invoke<{ ok: boolean; message: string }>(
        "cookbook_pull_model",
        { name: installed.name },
      );
      modelsChangedFlow.pullOk = result.ok;
      modelsChangedFlow.eventSeen = await eventArrived;
      modelsChangedFlow.detail = `re-pulled ${installed.name}: ok=${result.ok}; models:changed ${
        modelsChangedFlow.eventSeen ? "received" : "NOT received"
      }${result.ok ? "" : ` (${result.message.slice(0, 120)})`}`;
    } finally {
      unlisten();
    }
  } catch (e) {
    modelsChangedFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    modelsChangedFlow.settled = true;
  }
}

// Live flow exercise #2: the GLOBAL JOB STORE (state/jobs.ts). Drives a real
// Cookbook pull through `startCookbookPull` — the exact code path the panel's
// Pull button uses — and asserts the store-level contract the tab-switch fix
// depends on: the job appears in the module-scope store (so any tab can render
// it), a second start is a no-op, the backend in-flight registry
// (`cookbook_active_pulls`) sees it, the StatusBar pill paints, and on
// completion the job leaves the store and a `job`-source notification lands in
// the inbox. Runs strictly AFTER the models:changed flow (same model — the
// double-pull guard would otherwise reject one of them).
const jobStoreFlow = {
  attempted: false,
  settled: false,
  jobAppeared: false,
  secondStartIgnored: false,
  activePullsSeen: false,
  pillSeen: false,
  finishedClean: false,
  notificationRecorded: false,
  detail: "not attempted",
};

async function exerciseJobStoreFlow(): Promise<void> {
  if (jobStoreFlow.attempted) return;
  jobStoreFlow.attempted = true;
  try {
    const view = await invoke<{
      specs: { ollama_running: boolean };
      recommendations: Array<{ name: string; installed: boolean }>;
    }>("cookbook_recommendations");
    const installed = view.recommendations.find((r) => r.installed);
    if (!view.specs.ollama_running || !installed) {
      jobStoreFlow.detail = !view.specs.ollama_running
        ? "skipped: ollama not running"
        : "skipped: no installed model to re-pull";
      return;
    }
    const name = installed.name;
    const id = `pull:${name}`;

    const done = startCookbookPull(name); // fire; sample while it runs
    jobStoreFlow.jobAppeared = !!useJobs.getState().jobs[id];
    const startedAt = useJobs.getState().jobs[id]?.startedAt;
    await startCookbookPull(name); // second start must be a no-op
    jobStoreFlow.secondStartIgnored =
      useJobs.getState().jobs[id]?.startedAt === startedAt;

    // While in flight: backend registry + painted StatusBar pill. Polled —
    // a re-pull of an installed model can settle in a couple of seconds.
    const probeDeadline = Date.now() + 15_000;
    while (Date.now() < probeDeadline) {
      if (!jobStoreFlow.activePullsSeen) {
        try {
          const rows = await invoke<Array<{ name: string }>>("cookbook_active_pulls");
          if (rows.some((r) => r.name === name)) jobStoreFlow.activePullsSeen = true;
        } catch {
          /* keep polling */
        }
      }
      if (!jobStoreFlow.pillSeen && document.querySelector(".status-pill.jobs-pill")) {
        jobStoreFlow.pillSeen = true;
      }
      if (!useJobs.getState().jobs[id]) break; // settled — nothing left to observe
      if (jobStoreFlow.activePullsSeen && jobStoreFlow.pillSeen) break;
      await new Promise((r) => setTimeout(r, 100));
    }

    await done;
    jobStoreFlow.finishedClean = !useJobs.getState().jobs[id];
    jobStoreFlow.notificationRecorded = getNotificationsSnapshot().some(
      (n) => n.source === "job" && n.message.includes(name),
    );
    jobStoreFlow.detail =
      `pulled ${name} via job store: appeared=${jobStoreFlow.jobAppeared} ` +
      `dedup=${jobStoreFlow.secondStartIgnored} registry=${jobStoreFlow.activePullsSeen} ` +
      `pill=${jobStoreFlow.pillSeen} cleaned=${jobStoreFlow.finishedClean} ` +
      `notified=${jobStoreFlow.notificationRecorded}`;
  } catch (e) {
    jobStoreFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    jobStoreFlow.settled = true;
  }
}

// Live flow exercise #3: the EVAL/RESEARCH side of the job store (slice 2 of
// the migration) PLUS the eval model picker (P0-FINAL Wave 1: "benchmark the
// model you just pulled"). Drives a REAL benchmark run through `startEvalRun`
// — the exact code path the EvalPanel's button uses — with `persist: false`
// so probe runs never pollute the user's run history. The two tasks use the
// CORTEX_E2E-gated `[[e2e:echo]]`/`[[e2e:err]]` markers (everything else is
// the production path: real run_eval, real scoring, real events, real job
// store), which makes the flow fully offline-deterministic with BOTH
// verdicts: pass task answers "pong", fail task errors → report must come
// back 1/2. The requested model string must be echoed on the report — that's
// the new model-parameter plumbing end to end. Also proves the two in-flight
// registry commands (`eval_active`, `deep_research_active`) are registered:
// they're pure IPC, so a throw there is a wiring regression, never an
// environment problem. Finally, opens the REAL Eval tab and asserts the
// model picker rendered with the default-route option plus at least one
// pickable model group.
const EVAL_FLOW_TIMEOUT_MS = 60_000;
const evalJobStoreFlow = {
  attempted: false,
  settled: false,
  registryQueriesOk: false,
  jobAppeared: false,
  secondStartIgnored: false,
  activeSeen: false,
  finishedClean: false,
  reportInStore: false,
  notificationRecorded: false,
  modelEchoed: false,
  verdictsDeterministic: false,
  pickerRendered: false,
  realModelAttempted: false,
  realModelOk: false,
  realModelDetail: "not attempted",
  detail: "not attempted",
};

const EVAL_PROBE_MODEL = "e2e/probe-model";
/** The tag the Cookbook job-store flow (re-)pulls — locally served when present. */
const EVAL_REAL_MODEL = "ollama:llama3.2:1b";

async function exerciseEvalJobStoreFlow(): Promise<void> {
  if (evalJobStoreFlow.attempted) return;
  evalJobStoreFlow.attempted = true;
  try {
    // Both registry queries must answer (null = idle) — proves the commands
    // exist and the reload-adoption path has something to query.
    await invoke("eval_active");
    await invoke("deep_research_active");
    evalJobStoreFlow.registryQueriesOk = true;

    const id = "eval:run";
    const done = startEvalRun({
      tasks: [
        {
          id: "e2e-pass",
          prompt: "[[e2e:echo]] pong",
          expect_contains: ["pong"],
        },
        {
          id: "e2e-fail",
          prompt: "[[e2e:err]] deliberate failure",
          expect_contains: ["needle-that-cannot-match"],
        },
      ],
      model: EVAL_PROBE_MODEL,
      persist: false,
    });
    evalJobStoreFlow.jobAppeared = !!useJobs.getState().jobs[id];
    const startedAt = useJobs.getState().jobs[id]?.startedAt;
    await startEvalRun({ persist: false }); // second start must be a no-op
    evalJobStoreFlow.secondStartIgnored =
      useJobs.getState().jobs[id]?.startedAt === startedAt;

    // While in flight: the backend in-flight registry should see it. Polled —
    // a single trivial task can settle in well under a second when the
    // gateway answers fast (or refuses fast), so this is warn-level.
    const probeDeadline = Date.now() + 10_000;
    while (Date.now() < probeDeadline) {
      if (!useJobs.getState().jobs[id]) break; // settled
      try {
        if (await invoke("eval_active")) {
          evalJobStoreFlow.activeSeen = true;
          break;
        }
      } catch {
        /* keep polling */
      }
      await new Promise((r) => setTimeout(r, 100));
    }

    // Bounded wait: a black-hole gateway would otherwise hang the probe for
    // the HTTP client's full 600s timeout.
    const timedOut = await Promise.race([
      done.then(() => false),
      new Promise<boolean>((r) => setTimeout(() => r(true), EVAL_FLOW_TIMEOUT_MS)),
    ]);
    if (timedOut) {
      evalJobStoreFlow.detail = `error: eval run still in flight after ${EVAL_FLOW_TIMEOUT_MS / 1000}s (gateway black-holing?)`;
      return;
    }
    evalJobStoreFlow.finishedClean = !useJobs.getState().jobs[id];
    const report = useJobs.getState().evalRun.report;
    evalJobStoreFlow.reportInStore =
      !!report && report.results.some((r) => r.id === "e2e-pass");
    // The model param must round-trip onto the report — this is the new
    // adapter-registry plumbing (requested slug → report.model), checked
    // offline because the markers short-circuit before any adapter dials out.
    evalJobStoreFlow.modelEchoed = !!report && report.model === EVAL_PROBE_MODEL;
    // Both fake verdicts must land exactly: echo task passed (answer carries
    // "pong"), err task failed → 1/2. Any other outcome means scoring or the
    // marker gate regressed.
    const pass = report?.results.find((r) => r.id === "e2e-pass");
    const fail = report?.results.find((r) => r.id === "e2e-fail");
    evalJobStoreFlow.verdictsDeterministic =
      !!report &&
      report.total === 2 &&
      report.passed === 1 &&
      pass?.passed === true &&
      pass.answer.includes("pong") &&
      fail?.passed === false &&
      !!fail.error;
    evalJobStoreFlow.notificationRecorded = getNotificationsSnapshot().some(
      (n) => n.source === "job" && n.message.toLowerCase().includes("benchmark"),
    );

    // Real-model leg: prove the registry-routed path against a LIVE local
    // model — eval → orchestrator route → ollama adapter → real LLM, the
    // full "pull it, then benchmark it" flow with nothing faked. Only
    // attempted when the local Ollama actually serves the tag (warn-level in
    // the runner: environment-dependent), and bounded so a wedged server
    // can't stall the probe.
    try {
      const models = await invoke<Array<{ id: string }>>("list_models");
      if (Array.isArray(models) && models.some((m) => m.id === EVAL_REAL_MODEL)) {
        evalJobStoreFlow.realModelAttempted = true;
        const realDone = startEvalRun({
          tasks: [
            {
              id: "e2e-real",
              prompt: "Reply with exactly the single word: pong",
              expect_contains: ["pong"],
            },
          ],
          model: EVAL_REAL_MODEL,
          persist: false,
        });
        const realTimedOut = await Promise.race([
          realDone.then(() => false),
          new Promise<boolean>((r) => setTimeout(() => r(true), 45_000)),
        ]);
        const rep = useJobs.getState().evalRun.report;
        const res = rep?.results.find((r) => r.id === "e2e-real");
        evalJobStoreFlow.realModelOk =
          !realTimedOut &&
          rep?.model === EVAL_REAL_MODEL &&
          !!res &&
          !res.error &&
          res.answer.length > 0;
        evalJobStoreFlow.realModelDetail = realTimedOut
          ? "real-model leg timed out after 45s"
          : `model=${rep?.model} answer=${JSON.stringify((res?.answer ?? "").slice(0, 60))} ` +
            `error=${res?.error ?? "none"} passed=${res?.passed}`;
      } else {
        evalJobStoreFlow.realModelDetail = `skipped: ${EVAL_REAL_MODEL} not in model list`;
      }
    } catch (e) {
      evalJobStoreFlow.realModelDetail = `error: ${safeStringify(e).slice(0, 160)}`;
    }

    // UI side: the REAL Eval tab must render the model picker with the
    // default-route option plus at least one model (the curated gateway
    // catalog is static, so ≥2 options is environment-independent).
    const prevTab = useCortexStore.getState().activityTab;
    try {
      useCortexStore.getState().setActivityTab("eval");
      const deadline = Date.now() + 5_000;
      for (;;) {
        const select = document.querySelector<HTMLSelectElement>("select.eval-model-select");
        if (select && select.options.length >= 2) {
          evalJobStoreFlow.pickerRendered = true;
          break;
        }
        if (Date.now() > deadline) break;
        await new Promise((r) => setTimeout(r, 150));
      }
    } finally {
      useCortexStore.getState().setActivityTab(prevTab ?? null);
    }

    evalJobStoreFlow.detail =
      `ran 2-task marker benchmark via job store: registry=${evalJobStoreFlow.registryQueriesOk} ` +
      `appeared=${evalJobStoreFlow.jobAppeared} dedup=${evalJobStoreFlow.secondStartIgnored} ` +
      `active=${evalJobStoreFlow.activeSeen} cleaned=${evalJobStoreFlow.finishedClean} ` +
      `report=${evalJobStoreFlow.reportInStore} modelEchoed=${evalJobStoreFlow.modelEchoed} ` +
      `verdicts=${evalJobStoreFlow.verdictsDeterministic} picker=${evalJobStoreFlow.pickerRendered} ` +
      `notified=${evalJobStoreFlow.notificationRecorded}` +
      (report ? ` (passed=${report.passed}/${report.total} model=${report.model})` : "");
  } catch (e) {
    evalJobStoreFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    evalJobStoreFlow.settled = true;
  }
}

// Live flow exercise #4: KEEP-ALIVE for work-holding surfaces (P0-FINAL
// Wave 1 "tab switch destroys work"). Opens the REAL terminal tab — which
// spawns a real backend PTY — then switches tabs, closes the panel entirely,
// and comes back, asserting the pane is the SAME DOM node throughout (DOM
// identity ⇒ React never unmounted it ⇒ the unmount cleanup that kills the
// PTY never ran) and that the shell hasn't exited. Pre-fix, the first tab
// switch unmounted TerminalPane and closed the PTY.
const keepAliveFlow = {
  attempted: false,
  settled: false,
  terminalReady: false,
  survivedTabSwitch: false,
  survivedPanelClose: false,
  sameNodeAfterReturn: false,
  ptyAlive: false,
  detail: "not attempted",
};

async function exerciseKeepAliveFlow(): Promise<void> {
  if (keepAliveFlow.attempted) return;
  keepAliveFlow.attempted = true;
  const prevTab = useCortexStore.getState().activityTab;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  try {
    useCortexStore.getState().setActivityTab("terminal");

    // Wait for the PTY to come up: the pane exists and the "starting shell…"
    // status clears (the status row renders only while booting / on error).
    const deadline = Date.now() + 15_000;
    let pane: HTMLElement | null = null;
    for (;;) {
      pane = document.querySelector<HTMLElement>(".terminal-pane");
      const statusText =
        document.querySelector(".terminal-pane-status")?.textContent ?? "";
      if (statusText.includes("failed")) {
        keepAliveFlow.detail = `skipped: terminal failed to boot (${statusText.slice(0, 120)})`;
        return;
      }
      if (pane && !statusText.includes("starting")) break;
      if (Date.now() > deadline) {
        keepAliveFlow.detail = pane
          ? "error: shell still booting after 15s"
          : "error: terminal pane never mounted";
        return;
      }
      await sleep(150);
    }
    keepAliveFlow.terminalReady = true;
    pane.dataset.e2eKeepalive = "1"; // DOM-identity marker

    // 1. Switch to another tab — the pane must stay mounted, just hidden.
    useCortexStore.getState().setActivityTab("today");
    await sleep(250);
    const hidden = document.querySelector<HTMLElement>(".terminal-pane");
    keepAliveFlow.survivedTabSwitch =
      hidden?.dataset.e2eKeepalive === "1" && hidden.offsetParent === null;

    // 2. Close the panel entirely — previously App unmounted ActivityPanel.
    useCortexStore.getState().setActivityTab(null);
    await sleep(250);
    const closed = document.querySelector<HTMLElement>(".terminal-pane");
    keepAliveFlow.survivedPanelClose = closed?.dataset.e2eKeepalive === "1";

    // 3. Come back: same node, visible again, shell never exited.
    useCortexStore.getState().setActivityTab("terminal");
    await sleep(250);
    const back = document.querySelector<HTMLElement>(".terminal-pane");
    keepAliveFlow.sameNodeAfterReturn =
      back?.dataset.e2eKeepalive === "1" && back.offsetParent !== null;
    const finalStatus =
      document.querySelector(".terminal-pane-status")?.textContent ?? "";
    keepAliveFlow.ptyAlive =
      !finalStatus.includes("exited") && !finalStatus.includes("failed");
    keepAliveFlow.detail =
      `terminal keep-alive: ready=${keepAliveFlow.terminalReady} ` +
      `tabSwitch=${keepAliveFlow.survivedTabSwitch} panelClose=${keepAliveFlow.survivedPanelClose} ` +
      `sameNode=${keepAliveFlow.sameNodeAfterReturn} ptyAlive=${keepAliveFlow.ptyAlive}`;
  } catch (e) {
    keepAliveFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    try {
      useCortexStore.getState().setActivityTab(prevTab ?? null);
    } catch {
      /* restoring the tab is best-effort */
    }
    keepAliveFlow.settled = true;
  }
}

// Live flow exercise #5: Setup "Clone & connect" → project (P0-FINAL Wave 1).
// Drives a REAL `git clone` through the real `clone_git_repo` command against
// a throwaway local fixture repo (created by the e2e-gated backend helper
// under ~/.cortex/e2e/fixtures — NOT under any scan root, so its appearance
// in `list_projects` proves the new registry path, not the directory scan).
// Asserts the full hand-off the audit flagged as dead: clone → registered →
// `projects:changed` emitted → discoverable → `openProjectByPath` activates
// it, reveals the Projects sidebar, and the row actually paints. Cleanup
// removes the fixture, unregisters it, and restores the user's persisted
// active project (the backend helper snapshots/restores last-project.json).
const cloneConnectFlow = {
  attempted: false,
  settled: false,
  fixtureCreated: false,
  cloneOk: false,
  eventSeen: false,
  inProjectList: false,
  openedByPath: false,
  activeInStore: false,
  sidebarPainted: false,
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseCloneConnectFlow(): Promise<void> {
  if (cloneConnectFlow.attempted) return;
  cloneConnectFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const store = useCortexStore.getState;
  const prevTab = store().activityTab;
  const prevActive = store().activeProject;
  const prevSession = store().sessionId;
  let fixture: { src: string; dst: string } | null = null;
  try {
    fixture = await invoke<{ src: string; dst: string }>("e2e_make_clone_fixture");
    cloneConnectFlow.fixtureCreated = true;

    // Subscribe BEFORE cloning so the event can't race past us.
    let sawEvent: () => void = () => {};
    const eventArrived = new Promise<boolean>((resolve) => {
      sawEvent = () => resolve(true);
      setTimeout(() => resolve(false), 10_000);
    });
    const unlisten = await listen("projects:changed", () => sawEvent());
    try {
      const res = await invoke<{
        ok: boolean;
        project_root: string | null;
        stderr_tail: string;
        exit_code: number;
      }>("clone_git_repo", { url: `file://${fixture.src}`, targetDir: fixture.dst });
      cloneConnectFlow.cloneOk = res.ok && !!res.project_root;
      if (!cloneConnectFlow.cloneOk) {
        cloneConnectFlow.detail = `error: clone failed (exit ${res.exit_code}): ${res.stderr_tail.slice(0, 160)}`;
        return;
      }
      const root = res.project_root as string;
      cloneConnectFlow.eventSeen = await eventArrived;

      const projects = await invoke<Array<{ root: string; kind: string; group: string }>>(
        "list_projects",
      );
      cloneConnectFlow.inProjectList = projects.some(
        (p) => p.root === root && p.kind === "code",
      );

      // The real "Open project" hand-off: active project + Projects sidebar.
      cloneConnectFlow.openedByPath = await openProjectByPath(root);
      cloneConnectFlow.activeInStore =
        store().activeProject?.root === root && store().activityTab === "projects";

      // Painted ground truth: the sidebar row for the fixture goes active.
      const fixtureName = root.split("/").pop() ?? "";
      const paintDeadline = Date.now() + 5_000;
      while (Date.now() < paintDeadline) {
        const row = document.querySelector(".project-row.active");
        if (row?.textContent?.includes(fixtureName)) {
          cloneConnectFlow.sidebarPainted = true;
          break;
        }
        await sleep(150);
      }
      cloneConnectFlow.detail =
        `cloned ${fixtureName} via file://: clone=${cloneConnectFlow.cloneOk} ` +
        `event=${cloneConnectFlow.eventSeen} listed=${cloneConnectFlow.inProjectList} ` +
        `opened=${cloneConnectFlow.openedByPath} active=${cloneConnectFlow.activeInStore} ` +
        `painted=${cloneConnectFlow.sidebarPainted}`;
    } finally {
      unlisten();
    }
  } catch (e) {
    cloneConnectFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    // Cleanup + restore. The backend helper removes the fixture dirs,
    // unregisters the project, and puts last-project.json back; here we
    // restore the in-session store and re-point the backend at the previous
    // code project when there was one.
    try {
      if (fixture) {
        await invoke("e2e_cleanup_clone_fixture", { src: fixture.src, dst: fixture.dst });
      }
      // Opening the fixture bootstrapped a throwaway project session (one
      // system context message) — delete it so nightly runs don't accumulate
      // dead `clone-dst-*` rows in the sessions list.
      const fixtureSession = store().sessionId;
      if (fixtureSession && fixtureSession !== prevSession) {
        await invoke("e2e_delete_session", { sessionId: fixtureSession }).catch(() => {});
      }
      if (prevActive?.root && prevActive.kind === "code") {
        await invoke("set_active_project", { path: prevActive.root }).catch(() => {});
      }
      store().setActiveProject(prevActive ?? null);
      const refreshed = await invoke<ProjectMeta[]>("list_projects").catch(() => null);
      if (refreshed) store().setProjects(refreshed);
      cloneConnectFlow.cleanedUp = true;
    } catch {
      /* cleanup is best-effort; the registry self-prunes dead paths */
    }
    try {
      store().setActivityTab(prevTab ?? null);
    } catch {
      /* restoring the tab is best-effort */
    }
    cloneConnectFlow.settled = true;
  }
}

// Live flow exercise #6: ROUTINES run history → notifications → open-as-chat
// (P0-FINAL Wave 1). Drives two REAL runs through `run_routine_now` — one
// succeeding, one failing — using the CORTEX_E2E-gated `[[e2e:ok]]`/`[[e2e:err]]`
// fake-LLM markers (deterministic + offline; everything else is the production
// path: store writes, events, NotificationCenter bridge, session creation).
// Asserts the full chain the audit flagged dead: run → RoutineRun persisted in
// history (newest first, with trigger/duration) → `routines:run-recorded`
// emitted → `job`-source notification in the inbox from the module-scope
// bridge (failure = error severity) → `routine_run_as_session` materializes a
// chat session that the real `cortex:chat-replay` handler adopts into the
// store. Cleanup deletes both routines (purging their runs) + the session.
const routinesFlow = {
  attempted: false,
  settled: false,
  savedOk: false,
  runOk: false,
  eventSeen: false,
  historyRecorded: false,
  notificationRecorded: false,
  failureRecorded: false,
  openedAsChat: false,
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseRoutinesFlow(): Promise<void> {
  if (routinesFlow.attempted) return;
  routinesFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  interface SpecRow { id: string; name: string; last_status: string; last_error: string }
  interface RunRow {
    run_id: string; routine_id: string; status: string; output: string;
    error: string; trigger: string; duration_ms: number;
  }
  const NAME_OK = "e2e-probe-routine";
  const NAME_ERR = "e2e-probe-routine-fail";
  const blank = {
    id: "", prompt: "", interval_minutes: 0, enabled: true,
    last_run_unix_ms: 0, last_status: "", last_output: "", last_error: "",
  };
  let okId: string | null = null;
  let errId: string | null = null;
  let chatSession: string | null = null;
  const prevSession = useCortexStore.getState().sessionId;
  const prevMessages = useCortexStore.getState().messages;
  try {
    // 1. Create the succeeding routine (manual-only so the scheduler never
    //    double-fires it) and subscribe to the run-recorded event BEFORE
    //    running so it can't race past us.
    let specs = await invoke<SpecRow[]>("save_routine", {
      routine: { ...blank, name: NAME_OK, prompt: "[[e2e:ok]] reply pong" },
    });
    okId = specs.find((s) => s.name === NAME_OK)?.id ?? null;
    if (!okId) {
      routinesFlow.detail = "error: saved routine not in returned list";
      return;
    }
    routinesFlow.savedOk = true;

    let sawEvent: (run: RunRow) => void = () => {};
    const eventArrived = new Promise<RunRow | null>((resolve) => {
      sawEvent = (run) => resolve(run);
      setTimeout(() => resolve(null), 10_000);
    });
    const unlisten = await listen<RunRow>("routines:run-recorded", (e) => {
      if (e.payload.routine_id === okId) sawEvent(e.payload);
    });
    let spec: { last_status: string };
    try {
      spec = await invoke<{ last_status: string }>("run_routine_now", { id: okId });
      routinesFlow.runOk = spec.last_status === "ok";
      const evt = await eventArrived;
      routinesFlow.eventSeen = evt?.status === "ok" && evt.trigger === "manual";
    } finally {
      unlisten();
    }

    // 2. Persistent history: newest-first run record with the fake output.
    const runs = await invoke<RunRow[]>("list_routine_runs", { routineId: okId, limit: null });
    const newest = runs[0];
    routinesFlow.historyRecorded =
      !!newest && newest.status === "ok" && newest.trigger === "manual" &&
      newest.output.includes("e2e fake routine output");

    // 3. The module-scope bridge (armed by the always-mounted StatusBar) must
    //    have landed it in the NotificationCenter inbox. Polled briefly — the
    //    Tauri event fans out async relative to the invoke() resolution.
    const notifDeadline = Date.now() + 5_000;
    while (Date.now() < notifDeadline && !routinesFlow.notificationRecorded) {
      routinesFlow.notificationRecorded = getNotificationsSnapshot().some(
        (n) => n.source === "job" && n.ref === "routine" &&
          n.message.includes(NAME_OK) && n.severity === "info",
      );
      if (!routinesFlow.notificationRecorded) await sleep(150);
    }

    // 4. Failure path: recorded as an error run + error-severity notification.
    specs = await invoke<SpecRow[]>("save_routine", {
      routine: { ...blank, name: NAME_ERR, prompt: "[[e2e:err]]" },
    });
    errId = specs.find((s) => s.name === NAME_ERR)?.id ?? null;
    if (errId) {
      const failSpec = await invoke<SpecRow>("run_routine_now", { id: errId });
      const failRuns = await invoke<RunRow[]>("list_routine_runs", { routineId: errId, limit: null });
      const failNewest = failRuns[0];
      let failNotified = false;
      const failDeadline = Date.now() + 5_000;
      while (Date.now() < failDeadline && !failNotified) {
        failNotified = getNotificationsSnapshot().some(
          (n) => n.source === "job" && n.ref === "routine" &&
            n.message.includes(NAME_ERR) && n.severity === "error",
        );
        if (!failNotified) await sleep(150);
      }
      routinesFlow.failureRecorded =
        failSpec.last_status === "error" &&
        !!failNewest && failNewest.status === "error" &&
        failNewest.error.length > 0 && failNotified;
    }

    // 5. Open-as-chat: materialize the ok run as a session, then drive the
    //    REAL `cortex:chat-replay` handler and confirm the store adopted it.
    if (newest) {
      chatSession = await invoke<string>("routine_run_as_session", { runId: newest.run_id });
      window.dispatchEvent(
        new CustomEvent("cortex:chat-replay", { detail: { session_id: chatSession } }),
      );
      const chatDeadline = Date.now() + 8_000;
      while (Date.now() < chatDeadline && !routinesFlow.openedAsChat) {
        const st = useCortexStore.getState();
        routinesFlow.openedAsChat =
          st.sessionId === chatSession &&
          st.messages.some((m) => m.role === "assistant" && m.content.includes("e2e fake routine output")) &&
          st.messages.some((m) => m.role === "user" && m.content.includes(NAME_OK));
        if (!routinesFlow.openedAsChat) await sleep(150);
      }
    }

    routinesFlow.detail =
      `routines flow: saved=${routinesFlow.savedOk} run=${routinesFlow.runOk} ` +
      `event=${routinesFlow.eventSeen} history=${routinesFlow.historyRecorded} ` +
      `notified=${routinesFlow.notificationRecorded} failure=${routinesFlow.failureRecorded} ` +
      `chat=${routinesFlow.openedAsChat}`;
  } catch (e) {
    routinesFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    // Cleanup: delete_routine purges each routine's run history; the chat
    // session is e2e-deleted; the user's in-memory chat is restored.
    try {
      if (okId) await invoke("delete_routine", { id: okId });
      if (errId) await invoke("delete_routine", { id: errId });
      if (chatSession) {
        await invoke("e2e_delete_session", { sessionId: chatSession }).catch(() => {});
        useCortexStore.getState().adoptSession({
          ...(prevSession ? { sessionId: prevSession } : {}),
          messages: prevMessages,
        });
      }
      routinesFlow.cleanedUp = true;
    } catch {
      /* cleanup is best-effort */
    }
    routinesFlow.settled = true;
  }
}

// Live flow exercise #7: INLINE ASSIST (editor↔agent loop, P0-FINAL Wave 1).
// Drives the REAL `inline_assist` command end-to-end through the packaged
// binary using the CORTEX_E2E-gated `[[e2e:assist]]`/`[[e2e:assist-err]]`
// markers (deterministic + offline — only the LLM call itself is faked; the
// command registration, arg/result shapes, and error path are production
// code). The UI side (Ctrl+L mention + popover apply) is covered by the
// Playwright flow in scripts/e2e-editor-assist.mjs.
const inlineAssistFlow = {
  attempted: false,
  settled: false,
  okPath: false,
  errPath: false,
  detail: "not attempted",
};

async function exerciseInlineAssistFlow(): Promise<void> {
  if (inlineAssistFlow.attempted) return;
  inlineAssistFlow.attempted = true;
  try {
    const res = await invoke<{ replacement: string; model: string; latency_ms: number }>(
      "inline_assist",
      {
        args: {
          selection: "let total = a + b;",
          before: "fn sum(a: i64, b: i64) -> i64 {",
          after: "}",
          language: "Rust",
          instruction: "[[e2e:assist]] uppercase it",
          model: null,
          path: "/tmp/e2e-fake.rs",
        },
      },
    );
    inlineAssistFlow.okPath =
      res?.replacement === "LET TOTAL = A + B;" && res?.model === "e2e-fake";

    let errMessage = "";
    try {
      await invoke("inline_assist", {
        args: {
          selection: "x",
          before: "",
          after: "",
          language: null,
          instruction: "[[e2e:assist-err]]",
          model: null,
          path: null,
        },
      });
    } catch (e) {
      errMessage = safeStringify(e);
    }
    inlineAssistFlow.errPath = errMessage.includes("e2e fake assist failure");
    inlineAssistFlow.detail =
      `inline assist: ok=${inlineAssistFlow.okPath} (got "${res?.replacement}" via ${res?.model}) ` +
      `err=${inlineAssistFlow.errPath}`;
  } catch (e) {
    inlineAssistFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    inlineAssistFlow.settled = true;
  }
}

// Live flow exercise #8: ORCHESTRATOR TEAM RUN (P0-FINAL "Orchestrator is a
// static demo"). Drives the REAL `run_team` command — the exact path the
// dashboard's "Assign goal" modal invokes — through the full lifecycle in the
// running app: input validation, accept (team stamped `planning`), the
// in-flight double-start guard, `teams:updated` event delivery, and the run
// settling to a terminal `done|error` with the goal/tasks/transcripts
// persisted. The run goes through the production engine (manager plan →
// worker execution via the adapter registry), pointed at the local Ollama
// model the other flows already use — when that model answers, `runDone`
// additionally proves the whole plan→execute chain against a live LLM; when
// it can't, the run settles `error`, which still proves every wiring step
// (registration, managed state, persistence, events, lifecycle). Probe teams
// are named `e2e-probe-team` and deleted afterward, and their transcript
// sessions removed via `e2e_delete_session`, so nightly runs leave no residue.
const TEAM_FLOW_TIMEOUT_MS = 90_000;
const TEAM_PROBE_NAME = "e2e-probe-team";
const teamRunFlow = {
  attempted: false,
  settled: false,
  rejectsEmptyGoal: false,
  startAccepted: false,
  doubleStartBlocked: false,
  // False when the first run settled before the racing second start could
  // observe it in flight (instant adapter failure on a box without the local
  // model) — the guard is then untestable this run, not broken.
  doubleStartTested: false,
  eventSeen: false,
  liveProgressSeen: false,
  runSettled: false,
  settledStatus: "",
  runDone: false,
  workerTaskAssigned: false,
  transcriptsRecorded: false,
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseTeamRunFlow(): Promise<void> {
  if (teamRunFlow.attempted) return;
  teamRunFlow.attempted = true;
  const teams = await import("./teams");
  let teamId = "";
  let unlisten: (() => void) | undefined;
  try {
    // A blank goal must be refused before any side effect — pure IPC + input
    // validation, so this also proves the command is registered at all.
    try {
      await teams.runTeam("team-00000000", "   ");
    } catch (e) {
      teamRunFlow.rejectsEmptyGoal = safeStringify(e).toLowerCase().includes("goal");
    }

    // A leftover probe team from an interrupted earlier run would collide on
    // the name; it can't be mid-run anymore (the in-flight guard is in-memory
    // and this is a fresh process), so delete is safe.
    for (const t of await teams.listTeams()) {
      if (t.name === TEAM_PROBE_NAME) await teams.deleteTeam(t.id);
    }

    unlisten = await listen("teams:updated", () => {
      teamRunFlow.eventSeen = true;
    });

    const team = await teams.createTeam(TEAM_PROBE_NAME, "manager", ["worker"]);
    teamId = team.id;
    const goal = "Reply with the single word: ping.";
    const started = await teams.runTeam(teamId, goal, "ollama:llama3.2:1b");
    teamRunFlow.startAccepted =
      started.run_status === "planning" && started.goal === goal;
    try {
      // Same model on purpose: if the first run already settled (instant
      // adapter failure), this legitimately starts a second identical run and
      // the flow simply follows that one instead.
      await teams.runTeam(teamId, goal, "ollama:llama3.2:1b");
    } catch (e) {
      teamRunFlow.doubleStartTested = true;
      teamRunFlow.doubleStartBlocked = safeStringify(e)
        .toLowerCase()
        .includes("already running");
    }

    // Follow the run to a terminal state the way the dashboard does: by
    // re-reading the persisted team. Every failure path in the engine stamps
    // `error`, so a healthy build always settles well inside the bound.
    const deadline = Date.now() + TEAM_FLOW_TIMEOUT_MS;
    let last = started;
    while (Date.now() < deadline) {
      last = await teams.getTeam(teamId);
      if (last.workers.some((w) => w.status === "working" && !!w.current_task)) {
        teamRunFlow.liveProgressSeen = true;
      }
      if (last.run_status === "done" || last.run_status === "error") break;
      await new Promise((r) => setTimeout(r, 500));
    }
    teamRunFlow.settledStatus = last.run_status ?? "";
    teamRunFlow.runSettled =
      last.run_status === "done" || last.run_status === "error";
    teamRunFlow.runDone = last.run_status === "done";
    teamRunFlow.workerTaskAssigned = last.workers.every((w) => !!w.current_task);
    teamRunFlow.transcriptsRecorded =
      !!last.plan_session_id && last.workers.every((w) => !!w.session_id);

    // Leave no residue: drop the probe transcripts, then the team itself.
    if (teamRunFlow.runSettled) {
      const sessions = [
        last.plan_session_id,
        ...last.workers.map((w) => w.session_id),
      ].filter((s): s is string => !!s);
      for (const sid of sessions) {
        await invoke("e2e_delete_session", { sessionId: sid });
      }
      await teams.deleteTeam(teamId);
      teamId = "";
      teamRunFlow.cleanedUp = true;
    }

    teamRunFlow.detail =
      `team run via real run_team: rejectsEmptyGoal=${teamRunFlow.rejectsEmptyGoal} ` +
      `accepted=${teamRunFlow.startAccepted} ` +
      `guard=${teamRunFlow.doubleStartTested ? teamRunFlow.doubleStartBlocked : "untested (first run settled instantly)"} ` +
      `event=${teamRunFlow.eventSeen} settled=${teamRunFlow.settledStatus || "(in flight)"} ` +
      `tasks=${teamRunFlow.workerTaskAssigned} transcripts=${teamRunFlow.transcriptsRecorded} ` +
      `cleaned=${teamRunFlow.cleanedUp}`;
  } catch (e) {
    teamRunFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
    // Best-effort: never leave a probe team behind on an unexpected throw.
    if (teamId) {
      try {
        await teams.deleteTeam(teamId);
        teamRunFlow.cleanedUp = true;
      } catch {
        /* mid-run — the next probe's pre-clean will take it */
      }
    }
  } finally {
    unlisten?.();
    teamRunFlow.settled = true;
  }
}

// Live flow exercise: ORCHESTRATION SLICE 4 — code subtasks route through a
// worktree Lane. Drives the REAL `run_team` with a bound repo + the e2e-fake
// provider so a `code`-tagged worker is dispatched as a `lane_runs` row instead
// of a one-shot chat. The manager plan is deterministic: model `e2e-fake`
// routes (exact-id) to the fake adapter, which — seeing the planning prompt +
// the `[[e2e:team-code]]` marker — synthesizes a plan tagging the worker
// `code`/`hard`. The worker's code dispatch then creates a fake lane that
// auto-settles `done` through the production lane watcher. Proves the whole
// chain: plan → code tag → lane creation → lane_run_id linked onto the worker →
// lane reaches done → worker/run settle done. Probe team + its lane row +
// transcripts are cleaned up afterward, leaving no residue.
const TEAM_LANE_PROBE_NAME = "e2e-probe-lane-team";
const TEAM_LANE_TIMEOUT_MS = 60_000;
const teamCodeLaneFlow = {
  attempted: false,
  settled: false,
  startAccepted: false, // run_team accepted, team stamped planning
  runSettled: false, // run reached a terminal state
  runDone: false, // settled specifically `done`
  codeTagged: false, // manager tagged the worker `code`
  laneLinked: false, // worker.lane_run_id populated (no dead-end)
  laneReachedDone: false, // the linked lane_runs row settled `done`
  workerDone: false, // worker status reflects the lane outcome
  synthesisRecorded: false, // slice 5: the 2-worker run earned a synthesis pass
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseTeamCodeLaneFlow(): Promise<void> {
  if (teamCodeLaneFlow.attempted) return;
  teamCodeLaneFlow.attempted = true;
  const teams = await import("./teams");
  const mp = await import("./multi-provider");
  let teamId = "";
  let laneId = "";
  try {
    // Pre-clean a leftover probe team from an interrupted run.
    for (const t of await teams.listTeams()) {
      if (t.name === TEAM_LANE_PROBE_NAME) await teams.deleteTeam(t.id);
    }
    // Two workers: the marker tags the FIRST `code`/`hard` (→ lane), the second
    // stays `chat` (→ e2e-fake echo). A multi-worker run with real output is
    // what trips the slice-5 synthesis gate, so this one flow proves the lane
    // path AND the synthesis pass in a single deterministic run.
    const team = await teams.createTeam(TEAM_LANE_PROBE_NAME, "manager", [
      "coder",
      "reviewer",
    ]);
    teamId = team.id;
    // model `e2e-fake` → deterministic manager plan; `[[e2e:team-code]]` →
    // worker tagged code; repo bound → the lane dispatcher engages (forced to
    // the e2e-fake lane producer under CORTEX_E2E, so no gateway dialing).
    const goal = "Edit the repository. [[e2e:team-code]]";
    const started = await teams.runTeam(teamId, goal, "e2e-fake", "e2e/probe-lane-team");
    teamCodeLaneFlow.startAccepted = started.run_status === "planning";

    const deadline = Date.now() + TEAM_LANE_TIMEOUT_MS;
    let last = started;
    while (Date.now() < deadline) {
      last = await teams.getTeam(teamId);
      if (last.run_status === "done" || last.run_status === "error") break;
      await new Promise((r) => setTimeout(r, 400));
    }
    teamCodeLaneFlow.runSettled =
      last.run_status === "done" || last.run_status === "error";
    teamCodeLaneFlow.runDone = last.run_status === "done";

    const worker = last.workers[0];
    teamCodeLaneFlow.codeTagged = worker?.task_kind === "code";
    laneId = worker?.lane_run_id ?? "";
    teamCodeLaneFlow.laneLinked = laneId.length > 0;
    teamCodeLaneFlow.workerDone = worker?.status === "done";
    // Slice 5: the 2-worker run merges into a synthesis session (no dead-end).
    teamCodeLaneFlow.synthesisRecorded = !!last.synthesis_session_id;
    if (laneId) {
      const row = (await mp.listLaneRuns()).find((r) => r.run_id === laneId);
      teamCodeLaneFlow.laneReachedDone = row?.status === "done";
    }

    // Leave no residue: drop the lane row, the transcripts, then the team.
    if (teamCodeLaneFlow.runSettled) {
      if (laneId) {
        try {
          await mp.deleteLaneRun(laneId);
        } catch {
          /* still running guard — leave it for the next sweep */
        }
      }
      const sessions = [
        last.plan_session_id,
        last.synthesis_session_id,
        ...last.workers.map((w) => w.session_id),
      ].filter((s): s is string => !!s);
      for (const sid of sessions) {
        await invoke("e2e_delete_session", { sessionId: sid });
      }
      await teams.deleteTeam(teamId);
      teamId = "";
      teamCodeLaneFlow.cleanedUp = true;
    }

    teamCodeLaneFlow.detail =
      `team code-lane via real run_team: accepted=${teamCodeLaneFlow.startAccepted} ` +
      `codeTagged=${teamCodeLaneFlow.codeTagged} laneLinked=${teamCodeLaneFlow.laneLinked} ` +
      `laneDone=${teamCodeLaneFlow.laneReachedDone} workerDone=${teamCodeLaneFlow.workerDone} ` +
      `synthesis=${teamCodeLaneFlow.synthesisRecorded} ` +
      `run=${last.run_status ?? "(in flight)"} cleaned=${teamCodeLaneFlow.cleanedUp}`;
  } catch (e) {
    teamCodeLaneFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
    if (teamId) {
      try {
        await teams.deleteTeam(teamId);
        teamCodeLaneFlow.cleanedUp = true;
      } catch {
        /* mid-run — the next probe's pre-clean will take it */
      }
    }
  } finally {
    teamCodeLaneFlow.settled = true;
  }
}

// Focus-chain flow (Wave-2 "Focus chain can never populate"): drives ONE real
// `chat_send` turn at the e2e fake-LLM adapter (`e2e-fake`, registered only
// under CORTEX_E2E — see src-tauri/src/agents/e2e_fake.rs) on the LIVE
// session id. The fake streams a reply whose ```focus-chain fences are split
// across token chunks, so what's under test is the entire real chain: routing
// (explicit agent pick) → the chat event loop's FocusChainScanner → the
// synthetic `update_focus_chain` tool call → ChatPane's real listener →
// `replaceChain` → store + `save_focus_chain` persistence. Fully offline and
// deterministic. The probe's own listener provides the event-level asserts;
// the store/persistence asserts prove the REAL consumer handled it.
const FOCUS_FLOW_TIMEOUT_MS = 20_000;
const focusChainFlow = {
  attempted: false,
  settled: false,
  agentRouted: false, // chat_send accepted + routed to e2e-fake
  toolCallSeen: false, // synthetic update_focus_chain observed on the wire
  itemsCorrect: false, // final emission: 3 items, all done
  storeUpdated: false, // ChatPane's real handler landed it in the store
  persisted: false, // load_focus_chain round-trip from disk
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseFocusChainFlow(): Promise<void> {
  if (focusChainFlow.attempted) return;
  focusChainFlow.attempted = true;
  let unlisten: (() => void) | undefined;
  const sid = useCortexStore.getState().sessionId;
  try {
    if (!sid) {
      focusChainFlow.detail = "skip: no active session id";
      return;
    }
    // The real consumer under test is ChatPane's agent-event listener; without
    // a mounted composer there is nothing meaningful to assert against.
    if (!document.querySelector("textarea, [contenteditable=true]")) {
      focusChainFlow.detail = "skip: chat composer not mounted";
      return;
    }

    let lastItems: Array<{ title?: string; done?: boolean }> = [];
    let doneSeen = false;
    type AgentEvtPayload = {
      event?: {
        type?: string;
        name?: string;
        args?: { items?: Array<{ title?: string; done?: boolean }> };
      };
    };
    unlisten = await listen<AgentEvtPayload>(`agent-event:${sid}`, (e) => {
      const evt = e.payload?.event;
      if (evt?.type === "tool_call" && evt.name === "update_focus_chain") {
        focusChainFlow.toolCallSeen = true;
        lastItems = evt.args?.items ?? [];
      }
      if (evt?.type === "done") doneSeen = true;
    });

    const bridge = await import("./cortex-bridge");
    const res = await bridge.chatSend({
      sessionId: sid,
      message: "[[e2e:focus-chain]]",
      agent: "e2e-fake",
      history: [],
      mode: "act",
      model: null,
    });
    focusChainFlow.agentRouted = res.picked_agents.includes("e2e-fake");

    const deadline = Date.now() + FOCUS_FLOW_TIMEOUT_MS;
    while (Date.now() < deadline && !(doneSeen && focusChainFlow.toolCallSeen)) {
      await new Promise((r) => setTimeout(r, 200));
    }
    focusChainFlow.itemsCorrect =
      lastItems.length === 3 && lastItems.every((t) => t.done === true && !!t.title);

    // The store mutation runs in ChatPane's listener — a separate consumer of
    // the same event — so give it its own (short) settle window.
    const storeDeadline = Date.now() + 5_000;
    while (Date.now() < storeDeadline) {
      const chain = useCortexStore.getState().focusChain;
      if (chain.length === 3 && chain.every((t) => t.done)) {
        focusChainFlow.storeUpdated = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 200));
    }

    // replaceChain persists fire-and-forget; poll the disk round-trip briefly.
    const persistDeadline = Date.now() + 5_000;
    while (Date.now() < persistDeadline) {
      const onDisk = await invoke<Array<{ title: string; done: boolean }>>(
        "load_focus_chain",
        { sessionId: sid },
      );
      if (Array.isArray(onDisk) && onDisk.length === 3 && onDisk.every((t) => !!t.done)) {
        focusChainFlow.persisted = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 250));
    }

    focusChainFlow.detail =
      `real chat_send via e2e-fake on live session: routed=${focusChainFlow.agentRouted} ` +
      `toolCall=${focusChainFlow.toolCallSeen} items=${focusChainFlow.itemsCorrect} ` +
      `store=${focusChainFlow.storeUpdated} persisted=${focusChainFlow.persisted}`;
  } catch (e) {
    focusChainFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    unlisten?.();
    // Leave no residue: wipe the probe's chain (store + disk) and drop the
    // probe turn from the sessions store so nightly runs don't accumulate
    // fake transcripts. Best-effort — cleanup state is reported, not assumed.
    try {
      if (sid) {
        const fc = await import("./focus-chain");
        fc.clearChain();
        const after = await invoke<unknown[]>("load_focus_chain", { sessionId: sid });
        await invoke("e2e_delete_session", { sessionId: sid }).catch(() => {});
        focusChainFlow.cleanedUp = Array.isArray(after) && after.length === 0;
      }
    } catch {
      /* reported via cleanedUp=false */
    }
    focusChainFlow.settled = true;
  }
}

// Live flow exercise #10: LANES (P0-FINAL Wave-2 "Lanes: stop fire-and-forget").
// Drives the REAL `run_provider_lanes` → `lane_runs` persistence → watcher →
// `lanes:updated` → `list_lane_runs` chain the pane renders from, plus the
// stop/delete lifecycle, using the CORTEX_E2E-only fake providers (`e2e-fake`
// settles `done` through the production transition pipeline without dialing
// the gateway; `e2e-fake-hang` stays running so stop is provable). Offline and
// deterministic; probe rows are deleted afterwards so nightlies leave no
// residue in the lane history.
const LANES_FLOW_TIMEOUT_MS = 20_000;
const lanesFlow = {
  attempted: false,
  settled: false,
  rejectsEmptyProviders: false, // run_provider_lanes([]) refused pre-side-effect
  startAccepted: false, // dispatch returned a persisted `running` record
  eventSeen: false, // lanes:updated delivered
  persisted: false, // list_lane_runs serves the row (cross-tab source of truth)
  settledDone: false, // watcher folded the fake stream's Done into the row
  branchRecorded: false, // cortex/<run>/<provider> stamped on the row
  deleteGuardWorks: false, // delete refused while the lane is running
  stopWorks: false, // stop_lane_run → status `stopped`
  // Slice 2/2 — review & reattach (lane_review/merge_lane_run guards fire
  // before any Gitea I/O, so these are offline-deterministic):
  reviewGuardRunning: false, // lane_review refused on a running lane
  mergeCmdGuard: false, // merge_lane_run refused on an unknown lane (registered + guarded)
  interruptedSeeded: false, // e2e-fake-interrupt born `interrupted` (startup-sweep shape)
  reattachGuard: false, // reattach refused on a settled (done) lane
  reattachWorks: false, // interrupted → (reattach stream) → done; only the reattach
  // pipeline can move a row out of `interrupted`, so settling `done` proves the flip
  cleanedUp: false,
  detail: "not attempted",
};

async function exerciseLanesFlow(): Promise<void> {
  if (lanesFlow.attempted) return;
  lanesFlow.attempted = true;
  const mp = await import("./multi-provider");
  let doneId = "";
  let hangId = "";
  let intId = "";
  let unlisten: (() => void) | undefined;
  const findLane = async (id: string) =>
    (await mp.listLaneRuns()).find((r) => r.run_id === id);
  try {
    // Empty provider list must be refused before any row is written — pure
    // IPC validation, which also proves the command is registered.
    try {
      await mp.runProviderLanes("e2e", "probe", [], "x");
    } catch (e) {
      lanesFlow.rejectsEmptyProviders = safeStringify(e)
        .toLowerCase()
        .includes("provider");
    }

    unlisten = await listen("lanes:updated", () => {
      lanesFlow.eventSeen = true;
    });

    // Lane 1: the self-settling fake — proves dispatch → persist → watcher
    // transitions → done, all through the production pipeline.
    const recs = await mp.runProviderLanes(
      "e2e",
      "probe",
      ["e2e-fake"],
      "e2e probe lane (auto-settles)",
    );
    const rec = recs[0];
    doneId = rec?.run_id ?? "";
    lanesFlow.startAccepted = !!doneId && rec.status === "running";
    lanesFlow.branchRecorded = rec?.branch === `cortex/${doneId}/e2e-fake`;

    const deadline = Date.now() + LANES_FLOW_TIMEOUT_MS;
    while (Date.now() < deadline) {
      const row = await findLane(doneId);
      if (row) lanesFlow.persisted = true;
      if (row?.status === "done") {
        lanesFlow.settledDone = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 250));
    }

    // Lane 2: the hanging fake — proves the running-lane guard on delete and
    // the stop lifecycle the "Stop" button drives.
    const hung = await mp.runProviderLanes(
      "e2e",
      "probe",
      ["e2e-fake-hang"],
      "e2e probe lane (hangs until stopped)",
    );
    hangId = hung[0]?.run_id ?? "";
    if (hangId) {
      try {
        await mp.deleteLaneRun(hangId);
      } catch (e) {
        lanesFlow.deleteGuardWorks = safeStringify(e)
          .toLowerCase()
          .includes("running");
      }
      // Review is for settled lanes only — the guard fires before any Gitea
      // I/O, so this also proves `lane_review` is registered.
      try {
        await mp.laneReview(hangId);
      } catch (e) {
        lanesFlow.reviewGuardRunning = safeStringify(e)
          .toLowerCase()
          .includes("still running");
      }
      await mp.stopLaneRun(hangId);
      const stopDeadline = Date.now() + 5_000;
      while (Date.now() < stopDeadline) {
        const row = await findLane(hangId);
        if (row?.status === "stopped") {
          lanesFlow.stopWorks = true;
          break;
        }
        await new Promise((r) => setTimeout(r, 200));
      }
    }

    // merge_lane_run on an unknown lane must refuse before any Gitea I/O —
    // proves the command is registered and guarded.
    try {
      await mp.mergeLaneRun("lane-e2e-does-not-exist", 1);
    } catch (e) {
      lanesFlow.mergeCmdGuard = safeStringify(e).toLowerCase().includes("not found");
    }

    // Lane 3: born `interrupted` (the shape the startup sweep leaves after a
    // mid-run crash) — proves the reattach lifecycle the "Reattach" button
    // drives. Only `reattach_to_running` can move a row out of `interrupted`
    // (update_status treats it as terminal), so settling `done` afterwards
    // proves the full interrupted → running → done pipeline ran.
    const intr = await mp.runProviderLanes(
      "e2e",
      "probe",
      ["e2e-fake-interrupt"],
      "e2e probe lane (born interrupted)",
    );
    intId = intr[0]?.run_id ?? "";
    lanesFlow.interruptedSeeded = !!intId && intr[0]?.status === "interrupted";
    if (doneId) {
      try {
        await mp.reattachLaneRun(doneId); // settled `done` — must be refused
      } catch (e) {
        lanesFlow.reattachGuard = safeStringify(e)
          .toLowerCase()
          .includes("only interrupted");
      }
    }
    if (intId && lanesFlow.interruptedSeeded) {
      await mp.reattachLaneRun(intId);
      const reDeadline = Date.now() + 10_000;
      while (Date.now() < reDeadline) {
        const row = await findLane(intId);
        if (row?.status === "done") {
          lanesFlow.reattachWorks = true;
          break;
        }
        await new Promise((r) => setTimeout(r, 150));
      }
    }

    // Leave no residue: drop all probe rows from the lane history.
    for (const id of [doneId, hangId, intId]) {
      if (id) await mp.deleteLaneRun(id).catch(() => {});
    }
    lanesFlow.cleanedUp =
      !(await findLane(doneId)) &&
      (!hangId || !(await findLane(hangId))) &&
      (!intId || !(await findLane(intId)));

    lanesFlow.detail =
      `lanes via real run_provider_lanes: rejectsEmpty=${lanesFlow.rejectsEmptyProviders} ` +
      `accepted=${lanesFlow.startAccepted} event=${lanesFlow.eventSeen} ` +
      `persisted=${lanesFlow.persisted} done=${lanesFlow.settledDone} ` +
      `branch=${lanesFlow.branchRecorded} deleteGuard=${lanesFlow.deleteGuardWorks} ` +
      `stop=${lanesFlow.stopWorks} reviewGuard=${lanesFlow.reviewGuardRunning} ` +
      `mergeGuard=${lanesFlow.mergeCmdGuard} intSeeded=${lanesFlow.interruptedSeeded} ` +
      `reattachGuard=${lanesFlow.reattachGuard} reattach=${lanesFlow.reattachWorks} ` +
      `cleaned=${lanesFlow.cleanedUp}`;
  } catch (e) {
    lanesFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
    // Best-effort: never leave probe rows behind on an unexpected throw.
    for (const id of [doneId, hangId, intId]) {
      if (id) {
        await mp.stopLaneRun(id).catch(() => {});
        await mp.deleteLaneRun(id).catch(() => {});
      }
    }
  } finally {
    unlisten?.();
    lanesFlow.settled = true;
  }
}

// Live flow exercise: MULTIBUFFER "+ ADD EXCERPT" PICKER (P0-FINAL Wave 2
// "replace the path/range prompt dialogs with the QuickOpen file picker").
// Drives the REAL UI path promptAdd() uses: mounts the picker via
// `pickFileWithRange`, types an absolute path into the live React input (the
// pick-mode fallback row), fills the inline range field, clicks the row, and
// asserts the promise resolves with the parsed pick. Then lands the pick
// through the production `addExcerpt` helper against a real on-disk file
// (written via the production `save_file_text` command) and asserts the
// excerpt slice is exactly lines 10–20. All local-disk + DOM — fully
// offline-deterministic. Store/tab state is restored afterward.
const MB_PICK_FLOW_TIMEOUT_MS = 15_000;
// Fixture must live UNDER $HOME — the production `save_file_text` command
// (which `saveFileText` calls) refuses to write outside the home directory,
// so /tmp would be rejected. `~/.cortex/e2e/` already exists (the snapshot
// lives there) and is outside any project scan root.
const MB_PICK_BASENAME = "cortex-e2e-multibuffer.txt";
const multibufferPickFlow = {
  attempted: false,
  settled: false,
  modalRendered: false, // .quick-open-modal + search + range inputs painted
  fallbackRowShown: false, // typed absolute path produced a selectable row
  pickResolved: false, // promise resolved with the path + parsed 10:20 range
  excerptAdded: false, // addExcerpt landed exactly lines 10–20 in the store
  detail: "not attempted",
};

async function exerciseMultibufferPickFlow(): Promise<void> {
  if (multibufferPickFlow.attempted) return;
  multibufferPickFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (pred: () => boolean, ms: number) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      if (pred()) return true;
      await sleep(100);
    }
    return pred();
  };
  // Drive a controlled React input from outside React: the native value
  // setter + a bubbling `input` event is what React 18 listens for.
  const setReactInput = (el: HTMLInputElement, value: string) => {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const store = useCortexStore.getState();
  const prevExcerpts = store.multibufferExcerpts;
  const prevTab = store.activityTab;
  try {
    const { saveFileText } = await import("./editor-save");
    const { pickFileWithRange } = await import("./quick-open");
    const { addExcerpt } = await import("./multibuffer");
    const { homeDir, join } = await import("@tauri-apps/api/path");

    // A real file on disk through the production save path, under $HOME so the
    // home-directory write guard in `save_file_text` lets it through.
    const mbFile = await join(await homeDir(), ".cortex", "e2e", MB_PICK_BASENAME);
    const body = Array.from({ length: 60 }, (_, i) => `line ${i + 1}`).join("\n");
    await saveFileText(mbFile, body);

    const pickPromise = pickFileWithRange({ title: "E2E add excerpt" });
    await waitFor(() => !!document.querySelector(".quick-open-modal"), 5_000);
    const search = document.querySelector<HTMLInputElement>(".quick-open-search");
    const range = document.querySelector<HTMLInputElement>(".quick-open-range-input");
    multibufferPickFlow.modalRendered = !!search && !!range;

    if (search && range) {
      setReactInput(search, mbFile);
      multibufferPickFlow.fallbackRowShown = await waitFor(
        () =>
          Array.from(document.querySelectorAll(".quick-open-result")).some((r) =>
            (r.textContent ?? "").includes("cortex-e2e-multibuffer.txt"),
          ),
        5_000,
      );
      setReactInput(range, "10:20");
      const row = Array.from(
        document.querySelectorAll<HTMLElement>(".quick-open-result"),
      ).find((r) => (r.textContent ?? "").includes("cortex-e2e-multibuffer.txt"));
      row?.click();
    }

    const pick = await Promise.race([
      pickPromise,
      sleep(MB_PICK_FLOW_TIMEOUT_MS).then(() => null),
    ]);
    multibufferPickFlow.pickResolved =
      pick?.path === mbFile && pick?.range?.start === 10 && pick?.range?.end === 20;

    if (multibufferPickFlow.pickResolved && pick) {
      // The exact call promptAdd() makes with this pick.
      const ex = await addExcerpt(
        pick.path,
        pick.range?.start ?? 1,
        pick.range?.end ?? Number.MAX_SAFE_INTEGER,
      );
      const exLines = ex.body.split("\n");
      multibufferPickFlow.excerptAdded =
        ex.start_line === 10 &&
        ex.end_line === 20 &&
        exLines.length === 11 &&
        exLines[0] === "line 10" &&
        exLines[10] === "line 20";
    }

    multibufferPickFlow.detail =
      `picker via real pickFileWithRange + addExcerpt: modal=${multibufferPickFlow.modalRendered} ` +
      `fallbackRow=${multibufferPickFlow.fallbackRowShown} pick=${multibufferPickFlow.pickResolved} ` +
      `excerpt=${multibufferPickFlow.excerptAdded}`;
  } catch (e) {
    multibufferPickFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    // Never leave the modal up (it would shadow the rest of the snapshot) —
    // Escape goes through the modal's own window-level handler.
    if (document.querySelector('[data-cortex-mount="quick-open"]')) {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
      await sleep(100);
    }
    // Restore store/tab state (addExcerpt appends + switches to the tab).
    useCortexStore.getState().setMultibufferExcerpts(prevExcerpts);
    if (prevTab) useCortexStore.getState().setActivityTab(prevTab);
    multibufferPickFlow.settled = true;
  }
}

// Live flow exercise: GATEWAY GATING on the Deep Research surface (P0-FINAL
// Wave 4 "gate gateway-only features when standalone"). Deep research synthesizes
// reports with an LLM served by the gateway; with no gateway configured
// the composer must degrade to a humanized notice and disable the run controls
// instead of letting a doomed run start. This assertion is config-independent:
// it reads the real gateway base URL and asserts the rendered state MATCHES it
// — notice present ⟺ gateway missing — so it green-lights both a configured
// machine (notice absent, controls live) and a standalone one (notice shown,
// controls disabled).
const researchGateFlow = {
  attempted: false,
  settled: false,
  gatewayConfigured: false,
  noticeShown: false,
  runDisabled: false,
  inputDisabled: false,
  consistent: false,
  detail: "not attempted",
};

async function exerciseResearchGateFlow(): Promise<void> {
  if (researchGateFlow.attempted) return;
  researchGateFlow.attempted = true;
  const prevTab = useCortexStore.getState().activityTab;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  try {
    const { getGatewayConfig } = await import("./cortex-bridge");
    const cfg = await getGatewayConfig();
    const configured = cfg.base_url.trim().length > 0;
    researchGateFlow.gatewayConfigured = configured;

    useCortexStore.getState().setActivityTab("research");
    // Wait for the panel to mount (its run button is a stable anchor).
    const deadline = Date.now() + 5_000;
    let runBtn: HTMLButtonElement | null = null;
    for (;;) {
      runBtn = document.querySelector<HTMLButtonElement>(".research-run-btn");
      if (runBtn) break;
      if (Date.now() > deadline) break;
      await sleep(150);
    }
    // The gateway-configured check inside the panel is async; let it settle.
    await sleep(400);

    const notice = document.querySelector<HTMLElement>(".research-gateway-notice");
    const input = document.querySelector<HTMLTextAreaElement>(".research-input");
    researchGateFlow.noticeShown = !!notice;
    researchGateFlow.runDisabled = runBtn?.disabled === true;
    researchGateFlow.inputDisabled = input?.disabled === true;

    // Consistency: notice presence must mirror the missing-gateway condition.
    // When missing, both the input and run button must be disabled (dead-end
    // avoided). When configured, the notice must be absent and the input live.
    const missing = !configured;
    researchGateFlow.consistent = missing
      ? researchGateFlow.noticeShown &&
        researchGateFlow.inputDisabled &&
        researchGateFlow.runDisabled
      : !researchGateFlow.noticeShown && !researchGateFlow.inputDisabled;

    researchGateFlow.detail =
      `gateway ${configured ? "configured" : "MISSING"}: notice=${researchGateFlow.noticeShown} ` +
      `inputDisabled=${researchGateFlow.inputDisabled} runDisabled=${researchGateFlow.runDisabled} ` +
      `consistent=${researchGateFlow.consistent}`;
  } catch (e) {
    researchGateFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    useCortexStore.getState().setActivityTab(prevTab ?? null);
    researchGateFlow.settled = true;
  }
}

// ---- /duck gateway-gate flow -------------------------------------------
// The rubber-duck modal's Socratic questions are synthesized by an LLM served
// through the gateway. With no gateway configured (standalone build) the
// modal must degrade to a humanized notice + disabled composer instead of
// firing a doomed `duck_question` that returns a raw error. Same config-
// independent contract as researchGate: read the real gateway base URL,
// summon the modal, and assert the rendered state MIRRORS it — notice present
// ⟺ gateway missing, controls disabled when missing.
const duckGateFlow = {
  attempted: false,
  settled: false,
  gatewayConfigured: false,
  noticeShown: false,
  sendDisabled: false,
  inputDisabled: false,
  consistent: false,
  detail: "not attempted",
};

async function exerciseDuckGateFlow(): Promise<void> {
  if (duckGateFlow.attempted) return;
  duckGateFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  try {
    const { getGatewayConfig } = await import("./cortex-bridge");
    const cfg = await getGatewayConfig();
    const configured = cfg.base_url.trim().length > 0;
    duckGateFlow.gatewayConfigured = configured;

    const { openDuckChat } = await import("@/components/DuckChat");
    openDuckChat("e2e gateway gate check");
    // Wait for the portal modal to mount (the dialog is a stable anchor).
    const deadline = Date.now() + 5_000;
    let modal: HTMLElement | null = null;
    for (;;) {
      modal = document.querySelector<HTMLElement>(".duck-modal");
      if (modal) break;
      if (Date.now() > deadline) break;
      await sleep(150);
    }
    // The gateway check inside the modal is async; let it settle.
    await sleep(400);

    const notice = document.querySelector<HTMLElement>(".duck-gateway-notice");
    const input = document.querySelector<HTMLTextAreaElement>(".duck-input");
    const sendBtn = document.querySelector<HTMLButtonElement>(".duck-primary");
    duckGateFlow.noticeShown = !!notice;
    duckGateFlow.inputDisabled = input?.disabled === true;
    duckGateFlow.sendDisabled = sendBtn?.disabled === true;

    // When the gateway is missing, the notice must show and both composer
    // controls must be disabled (dead-end avoided). When configured, the
    // notice must be absent — we don't assert input state there because the
    // modal legitimately disables the composer while the opening question
    // streams from the real gateway.
    const missing = !configured;
    duckGateFlow.consistent = missing
      ? duckGateFlow.noticeShown &&
        duckGateFlow.inputDisabled &&
        duckGateFlow.sendDisabled
      : !duckGateFlow.noticeShown;

    duckGateFlow.detail =
      `gateway ${configured ? "configured" : "MISSING"}: notice=${duckGateFlow.noticeShown} ` +
      `inputDisabled=${duckGateFlow.inputDisabled} sendDisabled=${duckGateFlow.sendDisabled} ` +
      `consistent=${duckGateFlow.consistent}`;
  } catch (e) {
    duckGateFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    // Close the portal cleanly (the close button unmounts the detached root
    // and cancels any in-flight opening question via its cancelled flag).
    document.querySelector<HTMLButtonElement>(".duck-close")?.click();
    duckGateFlow.settled = true;
  }
}

// Live flow exercise: Git history "load-more + per-file diff navigation"
// (P0-FINAL Wave 5). The GitHistoryPanel pages commits via an offset cursor
// ("Load more") and, when a commit row is expanded, lists its files and renders
// a single file's diff on demand. This flow proves both halves end to end
// against a REAL multi-commit fixture repo and the production command layer:
//   • offset paging returns distinct, non-overlapping deeper pages (load-more),
//   • a multi-file commit reports every file it touched,
//   • a single file's diff comes back as a real unified diff with +/- rows,
//   • the actual GitHistoryPanel UI expands a commit and paints that file diff.
// Fully offline/deterministic (local git only), so every assertion is HARD.
const gitHistoryFlow = {
  attempted: false,
  settled: false,
  fixtureCreated: false,
  pageOneOk: false, // first page returned the expected commit count
  pageTwoDistinct: false, // deeper page has different commits (real pagination)
  pageThreeShorter: false, // last page is short → end-of-history reached
  multiFileListed: false, // multi-file commit lists both files it touched
  fileDiffOk: false, // single-file diff has the changed line + a hunk header
  uiDiffPainted: false, // the real panel expanded a commit + painted a diff
  detail: "not attempted",
};

async function exerciseGitHistoryFlow(): Promise<void> {
  if (gitHistoryFlow.attempted) return;
  gitHistoryFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (pred: () => boolean, ms: number) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      if (pred()) return true;
      await sleep(100);
    }
    return pred();
  };
  const store = useCortexStore.getState;
  const prevActive = store().activeProject;
  const prevTab = store().activityTab;
  let fixtureRoot: string | null = null;
  try {
    const fixture = await invoke<{
      root: string;
      multi_hash: string;
      edit_hash: string;
    }>("e2e_make_history_fixture");
    fixtureRoot = fixture.root;
    gitHistoryFlow.fixtureCreated = true;

    const { gitHistory, gitCommitFiles, gitCommitFileDiff } = await import("./git");

    // ── Load-more: page the 5-commit fixture two-at-a-time via the offset
    // cursor and confirm the deeper page is a DIFFERENT, non-overlapping slice
    // (exactly what the panel's "Load more" button does).
    const page1 = await gitHistory(fixture.root, 2, 0);
    const page2 = await gitHistory(fixture.root, 2, 2);
    const page3 = await gitHistory(fixture.root, 2, 4);
    gitHistoryFlow.pageOneOk = page1.length === 2;
    const p1 = new Set(page1.map((c) => c.hash));
    gitHistoryFlow.pageTwoDistinct =
      page2.length === 2 && page2.every((c) => !p1.has(c.hash));
    // 5 commits, paged by 2: offset 4 yields the final lone commit.
    gitHistoryFlow.pageThreeShorter = page3.length === 1;

    // ── Per-commit file list: the multi-file commit touched a.txt's siblings
    // b.txt + c.txt; both must surface.
    const files = await gitCommitFiles(fixture.root, fixture.multi_hash);
    const paths = new Set(files.map((f) => f.path));
    gitHistoryFlow.multiFileListed =
      files.length === 2 && paths.has("b.txt") && paths.has("c.txt");

    // ── Per-file diff: the edit commit changed one line of a.txt; its
    // single-file diff must carry a hunk header and the changed text.
    const diff = await gitCommitFileDiff(fixture.root, fixture.edit_hash, "a.txt");
    gitHistoryFlow.fileDiffOk =
      diff.includes("@@") && diff.includes("alpha two CHANGED");

    // ── UI: drive the REAL GitHistoryPanel — point it at the fixture, open the
    // git tab, expand the edit commit, click a.txt, and assert the diff paints.
    // The GitHistoryPanel only reads `project.root`, but the store wants a full
    // ProjectMeta — synthesize one for the fixture (UI-only, never persisted).
    store().setActiveProject({
      root: fixture.root,
      name: "e2e-history-fixture",
      has_claude_md: false,
      has_git: true,
      has_runbooks: false,
      last_modified_ms: 0,
      group: "e2e",
      kind: "code",
      note_path: null,
      subtitle: null,
    });
    store().setActivityTab("git");
    const shortEdit = fixture.edit_hash.slice(0, 7);
    const rowsReady = await waitFor(
      () => document.querySelectorAll(".git-commit-row").length >= 5,
      8_000,
    );
    if (rowsReady) {
      const editRow = Array.from(
        document.querySelectorAll<HTMLElement>(".git-commit-row"),
      ).find((r) => (r.textContent ?? "").includes(shortEdit));
      editRow?.click();
      await waitFor(
        () => document.querySelectorAll(".git-commit-file").length > 0,
        5_000,
      );
      const fileBtn = Array.from(
        document.querySelectorAll<HTMLElement>(".git-commit-file"),
      ).find((b) => (b.textContent ?? "").includes("a.txt"));
      fileBtn?.click();
      gitHistoryFlow.uiDiffPainted = await waitFor(() => {
        const diffEl = document.querySelector(".git-file-diff");
        return (
          !!diffEl &&
          (diffEl.textContent ?? "").includes("alpha two CHANGED") &&
          diffEl.querySelectorAll(".hunk-row-add").length > 0
        );
      }, 5_000);
    }

    gitHistoryFlow.detail =
      `fixture=${gitHistoryFlow.fixtureCreated} page1=${gitHistoryFlow.pageOneOk} ` +
      `page2distinct=${gitHistoryFlow.pageTwoDistinct} page3short=${gitHistoryFlow.pageThreeShorter} ` +
      `multiFile=${gitHistoryFlow.multiFileListed} fileDiff=${gitHistoryFlow.fileDiffOk} ` +
      `uiDiff=${gitHistoryFlow.uiDiffPainted}`;
  } catch (e) {
    gitHistoryFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    // Restore the user's project + tab, then delete the fixture repo.
    try {
      if (prevActive?.root && prevActive.kind === "code") {
        await invoke("set_active_project", { path: prevActive.root }).catch(() => {});
      }
      store().setActiveProject(prevActive ?? null);
      store().setActivityTab(prevTab ?? null);
      if (fixtureRoot) {
        await invoke("e2e_cleanup_history_fixture", { root: fixtureRoot }).catch(
          () => {},
        );
      }
    } catch {
      /* best-effort cleanup */
    }
    gitHistoryFlow.settled = true;
  }
}

// ── Help panel — live slash-command reference ───────────────────────────────
// The Help tab's "Slash commands" section used to be a hand-curated subset of
// ~13 commands; it now renders the REAL command registry (`COMMANDS`) grouped
// by category, so it can never drift from what the composer actually accepts.
// This flow drives the actual HelpPanel UI: open the Help tab, expand the
// section, and assert the registry rendered in full (every command paints a
// row) plus that the live filter narrows and clears correctly. Fully offline
// and deterministic, so every assertion is HARD.
const helpReferenceFlow = {
  attempted: false,
  settled: false,
  panelMounted: false,
  sectionExpanded: false,
  rowsRendered: false, // a row painted for every registered command
  filterNarrows: false, // typing a query reduces the visible rows to matches
  filterEmptyState: false, // a no-match query shows the humanized empty state
  filterCleared: false, // clearing the filter restores the full list
  registeredCount: 0,
  renderedCount: 0,
  detail: "not attempted",
};

async function exerciseHelpReferenceFlow(): Promise<void> {
  if (helpReferenceFlow.attempted) return;
  helpReferenceFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (pred: () => boolean, ms: number) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      if (pred()) return true;
      await sleep(60);
    }
    return pred();
  };
  const setReactInput = (el: HTMLInputElement, value: string) => {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const store = useCortexStore.getState;
  const prevTab = store().activityTab;
  try {
    // The source of truth the panel renders from — unique canonical names.
    const { COMMANDS } = await import("./slash-commands");
    const registered = new Set(COMMANDS.map((c) => c.name));
    helpReferenceFlow.registeredCount = registered.size;

    store().setActivityTab("help");
    helpReferenceFlow.panelMounted = await waitFor(
      () => !!document.querySelector(".help-panel"),
      4000,
    );
    if (!helpReferenceFlow.panelMounted) {
      helpReferenceFlow.detail = "error: help panel never mounted";
      return;
    }

    // Expand the (collapsed-by-default) "Slash commands" section by clicking
    // its real header button — exactly what a user does.
    const heads = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".help-section-head"),
    );
    const slashHead = heads.find(
      (h) => h.querySelector(".help-section-title")?.textContent?.trim() === "Slash commands",
    );
    if (!slashHead) {
      helpReferenceFlow.detail = "error: 'Slash commands' section header not found";
      return;
    }
    slashHead.click();
    helpReferenceFlow.sectionExpanded = await waitFor(
      () => document.querySelectorAll(".help-cmd-row").length > 0,
      3000,
    );

    const countRows = () => document.querySelectorAll(".help-cmd-name").length;
    helpReferenceFlow.renderedCount = countRows();
    // Every registered command must paint a row — the whole point of going live.
    helpReferenceFlow.rowsRendered =
      helpReferenceFlow.renderedCount === registered.size && registered.size > 20;

    const filter = document.querySelector<HTMLInputElement>(".help-cmd-filter");
    if (filter) {
      // Narrow to a command we know exists in the registry.
      const probe = registered.has("commit") ? "commit" : COMMANDS[0]?.name ?? "help";
      setReactInput(filter, probe);
      await waitFor(() => countRows() < helpReferenceFlow.renderedCount, 1500);
      const narrowed = countRows();
      helpReferenceFlow.filterNarrows = narrowed > 0 && narrowed < helpReferenceFlow.renderedCount;

      // A query that can't match anything → the humanized empty state.
      setReactInput(filter, "zzzznotacommandzzzz");
      helpReferenceFlow.filterEmptyState = await waitFor(
        () => !!document.querySelector(".help-cmd-empty") && countRows() === 0,
        1500,
      );

      // Clearing restores the full list.
      setReactInput(filter, "");
      helpReferenceFlow.filterCleared = await waitFor(
        () => countRows() === helpReferenceFlow.renderedCount,
        1500,
      );
    }

    helpReferenceFlow.detail =
      `help reference: mounted=${helpReferenceFlow.panelMounted} ` +
      `expanded=${helpReferenceFlow.sectionExpanded} ` +
      `rows=${helpReferenceFlow.renderedCount}/${helpReferenceFlow.registeredCount} ` +
      `filterNarrows=${helpReferenceFlow.filterNarrows} empty=${helpReferenceFlow.filterEmptyState} ` +
      `cleared=${helpReferenceFlow.filterCleared}`;
  } catch (e) {
    helpReferenceFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    try {
      store().setActivityTab(prevTab ?? null);
    } catch {
      /* restoring the tab is best-effort */
    }
    helpReferenceFlow.settled = true;
  }
}

// ── Help panel — live @-mention reference ────────────────────────────────────
// The Help tab's "@-mentions" section used to be two hand-written lists that had
// drifted from reality (one referenced a nonexistent "LSP index"; both omitted
// the fully-shipped `@codebase` provider). It now renders the REAL provider
// registry (`AT_PROVIDERS`) grouped by category, so it can never drift from what
// the composer resolves. This flow drives the actual HelpPanel UI: open the Help
// tab, expand the "@-mentions" section, and assert (a) a row painted for EVERY
// provider in the registry, (b) the flagship providers — crucially `@codebase`
// and `@docs`, the regression that motivated this — are all present, and (c) the
// section's own live filter narrows / empty-states / clears. Fully offline and
// deterministic, so every assertion is HARD.
const atVocabFlow = {
  attempted: false,
  settled: false,
  panelMounted: false,
  sectionExpanded: false,
  rowsRendered: false, // a row painted for every registered provider
  flagshipsPresent: false, // every sentinel provider (incl. @codebase/@docs) rendered
  filterNarrows: false,
  filterEmptyState: false,
  filterCleared: false,
  registeredCount: 0,
  renderedCount: 0,
  missingFlagships: [] as string[],
  detail: "not attempted",
};

async function exerciseAtVocabReferenceFlow(): Promise<void> {
  if (atVocabFlow.attempted) return;
  atVocabFlow.attempted = true;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (pred: () => boolean, ms: number) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      if (pred()) return true;
      await sleep(60);
    }
    return pred();
  };
  const setReactInput = (el: HTMLInputElement, value: string) => {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const store = useCortexStore.getState;
  const prevTab = store().activityTab;
  try {
    // The source of truth the panel renders from.
    const { AT_PROVIDERS } = await import("./at-vocab");
    atVocabFlow.registeredCount = AT_PROVIDERS.length;

    store().setActivityTab("help");
    atVocabFlow.panelMounted = await waitFor(
      () => !!document.querySelector(".help-panel"),
      4000,
    );
    if (!atVocabFlow.panelMounted) {
      atVocabFlow.detail = "error: help panel never mounted";
      return;
    }

    // Expand the (collapsed-by-default) "@-mentions" section by clicking its
    // real header button — exactly what a user does.
    const heads = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".help-section-head"),
    );
    const atHead = heads.find(
      (h) => h.querySelector(".help-section-title")?.textContent?.trim() === "@-mentions",
    );
    if (!atHead) {
      atVocabFlow.detail = "error: '@-mentions' section header not found";
      return;
    }
    atHead.click();
    atVocabFlow.sectionExpanded = await waitFor(
      () => document.querySelectorAll(".help-at-body .help-cmd-row").length > 0,
      3000,
    );

    // Count within the @-section's own container so the slash-command rows
    // (different section, same row class) can never be miscounted here.
    const names = () =>
      Array.from(document.querySelectorAll(".help-at-body .help-cmd-name")).map(
        (n) => n.textContent?.trim() ?? "",
      );
    const countRows = () => names().length;
    atVocabFlow.renderedCount = countRows();
    // Every registered provider must paint a row — the whole point of going live.
    atVocabFlow.rowsRendered =
      atVocabFlow.renderedCount === AT_PROVIDERS.length && AT_PROVIDERS.length > 15;

    // The flagship providers must all be discoverable. `@codebase` and `@docs`
    // are the two that had shipped fully wired yet undocumented — the exact
    // regression this guard exists to catch.
    const SENTINELS = [
      "@codebase",
      "@docs",
      "@repomap",
      "@outline:<file>",
      "@def:<symbol>",
      "@refs:<symbol>",
      "@grep:<pattern>",
      "@web:<url>",
      "@websearch:<query>",
      "@folder:<path>",
      "@tree",
      "@diff",
      "@status",
      "@log",
      "@blame:<file>",
      "@terminal",
      "@problems",
      "@brain",
      "@env",
      "@frag:<name>",
    ];
    const rendered = new Set(names());
    atVocabFlow.missingFlagships = SENTINELS.filter((s) => !rendered.has(s));
    atVocabFlow.flagshipsPresent = atVocabFlow.missingFlagships.length === 0;

    const filter = document.querySelector<HTMLInputElement>(
      ".help-at-body .help-cmd-filter",
    );
    if (filter) {
      // Narrow to a provider we know exists in the registry.
      setReactInput(filter, "codebase");
      await waitFor(() => countRows() < atVocabFlow.renderedCount, 1500);
      const narrowed = countRows();
      atVocabFlow.filterNarrows = narrowed > 0 && narrowed < atVocabFlow.renderedCount;

      // A query that can't match anything → the humanized empty state.
      setReactInput(filter, "zzzznotaproviderzzzz");
      atVocabFlow.filterEmptyState = await waitFor(
        () =>
          !!document.querySelector(".help-at-body .help-cmd-empty") && countRows() === 0,
        1500,
      );

      // Clearing restores the full list.
      setReactInput(filter, "");
      atVocabFlow.filterCleared = await waitFor(
        () => countRows() === atVocabFlow.renderedCount,
        1500,
      );
    }

    atVocabFlow.detail =
      `at-vocab reference: mounted=${atVocabFlow.panelMounted} ` +
      `expanded=${atVocabFlow.sectionExpanded} ` +
      `rows=${atVocabFlow.renderedCount}/${atVocabFlow.registeredCount} ` +
      `flagships=${atVocabFlow.flagshipsPresent}` +
      (atVocabFlow.missingFlagships.length
        ? ` missing=[${atVocabFlow.missingFlagships.join(",")}]`
        : "") +
      ` filterNarrows=${atVocabFlow.filterNarrows} empty=${atVocabFlow.filterEmptyState} ` +
      `cleared=${atVocabFlow.filterCleared}`;
  } catch (e) {
    atVocabFlow.detail = `error: ${safeStringify(e).slice(0, 200)}`;
  } finally {
    try {
      store().setActivityTab(prevTab ?? null);
    } catch {
      /* restoring the tab is best-effort */
    }
    atVocabFlow.settled = true;
  }
}

// What the renderer can introspect about its own paint state. The runner keys
// its verdict off these.
async function collectSnapshot(): Promise<Record<string, unknown>> {
  const root = document.getElementById("root");
  const rootChildren = root?.childElementCount ?? 0;
  const style = getComputedStyle(document.documentElement);
  const bodyStyle = getComputedStyle(document.body);

  // IMPORTANT: capture every DOM/style read SYNCHRONOUSLY, up front, before any
  // `await` below. `collectSnapshot` awaits IPC (active theme, gateway) and the
  // theme applies asynchronously on boot (`useThemeBoot`), so reads taken across
  // those awaits would tear — early fields catching the pre-theme default and
  // late fields the applied theme, an internally-inconsistent snapshot. Reading
  // it all here makes each snapshot a single coherent instant.
  const rootInlineStyle = document.documentElement.style;
  // The active theme's tokens as the appliers actually wrote them: inline custom
  // properties on `:root`. Source of truth for "which theme is applied"; falls
  // back to the computed value for any engine/timing where the inline is empty.
  const rootInlineBg = rootInlineStyle.getPropertyValue("--bg").trim();
  const rootInlineAccent = rootInlineStyle.getPropertyValue("--accent").trim();
  // What getComputedStyle(:root) actually RESOLVES the core tokens to. The
  // appliers write the active palette inline on `document.documentElement`
  // (= `:root`), and an inline custom property on the root element DOES surface
  // in its own computed style — so the computed value reflects the ACTIVE theme,
  // not the `global.css` default block. These are the ground truth for
  // "`:root` itself reflects the active theme"; the active==painted check below
  // asserts against them (not the inline fallback) so a regression that left the
  // tokens off `:root` would actually fail.
  const rootComputedBg = style.getPropertyValue("--bg").trim();
  const rootComputedAccent = style.getPropertyValue("--accent").trim();
  // Prefer the computed `:root` value (the real cascade result); fall back to the
  // raw inline write only if computed is somehow empty (e.g. a probe firing in a
  // teardown frame after the inline was cleared).
  const cssBg = rootComputedBg || rootInlineBg;
  const cssAccent = rootComputedAccent || rootInlineAccent;
  // The body renders its background from `var(--bg)`, so its resolved
  // background-color is the ground truth for what's actually PAINTED on screen.
  const paintedBodyBg = bodyStyle.backgroundColor.trim();
  const paintedBodyColor = bodyStyle.color;
  const customThemeAttr = document.documentElement.dataset.customTheme ?? "";
  const themeModeAttr = document.documentElement.dataset.themeMode ?? "";
  const legacyThemeAttr = document.documentElement.dataset.theme ?? "";
  const totalNodes = document.getElementsByTagName("*").length;
  const hasChatComposer = !!document.querySelector("textarea, [contenteditable=true]");
  // The status-bar model strip is the painted ground truth for what
  // `list_models` discovered (Claude CLI + Cortex Gateway + Ollama union) — lets the
  // runner assert a freshly pulled local model is actually visible in the UI.
  const modelStripPills = Array.from(document.querySelectorAll(".model-pill"))
    .slice(0, 40)
    .map((el) => (el.textContent ?? "").trim())
    .filter(Boolean);
  // The strip hides entirely in compact status-bar mode (a persisted user
  // preference), so the composer's always-mounted ModelPicker options are the
  // reliable painted ground truth for "this model is pickable right now".
  const modelPickerOptions = Array.from(
    document.querySelectorAll<HTMLOptionElement>(".model-picker-select option"),
  )
    .slice(0, 60)
    .map((o) => o.value)
    .filter(Boolean);
  const headings = Array.from(document.querySelectorAll("h1,h2,h3,[role=heading]"))
    .slice(0, 8)
    .map((el) => (el.textContent ?? "").trim())
    .filter(Boolean);

  let activeTheme = "";
  try {
    const ts = await getActiveThemeState();
    activeTheme = ts.active ?? "";
  } catch {
    /* themes are best-effort */
  }

  // active==painted assertion: resolve the active theme's *intended* tokens and
  // compare them to what's actually PAINTED (cssBg/cssAccent above). This catches
  // the flash/divergence class of bug (a stale or mismatched theme on screen while
  // the backend reports a different active name) that a mere "vars are non-empty"
  // check can't see. Only meaningful when an active theme name resolved; null
  // otherwise (default sheet, nothing to compare against).
  const norm = (v: string) => v.trim().toLowerCase().replace(/\s+/g, "");
  // #rrggbb / #rgb → "rgb(r, g, b)" so a hex expectation can be compared against
  // a computed `background-color` (which the engine always reports as rgb()).
  const hexToRgb = (hex: string): string | null => {
    let h = hex.trim().replace(/^#/, "");
    if (h.length === 3) h = h.split("").map((c) => c + c).join("");
    if (h.length !== 6 || /[^0-9a-fA-F]/.test(h)) return null;
    const r = parseInt(h.slice(0, 2), 16);
    const g = parseInt(h.slice(2, 4), 16);
    const b = parseInt(h.slice(4, 6), 16);
    return `rgb(${r}, ${g}, ${b})`;
  };
  let expectedBg = "";
  let expectedAccent = "";
  let themeMatches: boolean | null = null;
  // Whether getComputedStyle(:root) itself resolves to the active theme's core
  // tokens (vs. the global.css default block). Null when no active theme is set.
  let rootReflectsActive: boolean | null = null;
  if (activeTheme) {
    try {
      const resolved = await resolveTheme(activeTheme);
      expectedBg = resolved.bg ?? "";
      expectedAccent = resolved.accent ?? "";
      if (expectedBg && expectedAccent) {
        // active==painted now requires THREE things, all on the active theme:
        //   1. `:root` itself reflects it — getComputedStyle(:root)'s --bg/--accent
        //      resolve to the active palette (rootReflectsActive). This is the
        //      tightened assertion: the tokens must land on :root, not just an
        //      inline write we read back ourselves.
        //   2. the resolved `cssBg`/`cssAccent` (computed-first) equal the active
        //      theme, and
        //   3. the screen actually shows them — the body's painted
        //      background-color equals the active theme's bg.
        const expectedBgRgb = hexToRgb(expectedBg);
        rootReflectsActive =
          norm(rootComputedBg) === norm(expectedBg) &&
          norm(rootComputedAccent) === norm(expectedAccent);
        const appliedMatches =
          norm(cssBg) === norm(expectedBg) && norm(cssAccent) === norm(expectedAccent);
        const paintMatches =
          !expectedBgRgb || norm(paintedBodyBg) === norm(expectedBgRgb);
        themeMatches = rootReflectsActive && appliedMatches && paintMatches;
      }
    } catch {
      /* theme resolution is best-effort; leave themeMatches null */
    }
  }

  // Live gateway reachability — the same backend call the status bar uses.
  let gateway: unknown = null;
  try {
    gateway = await invoke("gateway_status");
  } catch (e) {
    gateway = { error: safeStringify(e) };
  }

  const store = useCortexStore.getState();

  return {
    ts: Date.now(),
    url: location.href,
    // Liveness / paint
    dom: {
      rootMounted: rootChildren > 0,
      rootChildren,
      totalNodes,
      bodyBg: paintedBodyBg,
      bodyColor: paintedBodyColor,
      visibleHeadings: headings,
      hasChatComposer,
      modelStripPills,
      modelPickerOptions,
    },
    // Theme-on-launch regression guard (the bug fixed alongside the black screen)
    theme: {
      activeName: activeTheme,
      cssBg,
      cssAccent,
      cssVarsApplied: cssBg.length > 0 && cssAccent.length > 0,
      // active==painted: the tokens the active theme INTENDS vs what's PAINTED.
      // `themeMatches` is true/false when an active theme resolved, null when no
      // active theme is set (default sheet — nothing to compare against).
      expectedBg,
      expectedAccent,
      themeMatches,
      // Tightened active==painted sub-signal: does getComputedStyle(:root)
      // itself resolve to the active theme's --bg/--accent? True means `:root`
      // reflects the active theme (not the global.css default block); false
      // would be the exact regression this guards against. Null when no active
      // theme is set. `themeMatches` now requires this to be true.
      rootReflectsActive,
      // Divergence diagnostics: where the token actually lives. `rootInlineBg`
      // is what the appliers wrote; `rootComputedBg`/`rootComputedAccent` are
      // what getComputedStyle(:root) reports — and since the appliers write
      // inline on :root, the computed values reflect the ACTIVE theme. A mismatch
      // between the computed values and the active theme is a real applier/cascade
      // bug; an all-default snapshot (inline + computed both empty) means it was
      // taken before the boot theme settled.
      rootInlineBg,
      rootInlineAccent,
      rootComputedBg,
      rootComputedAccent,
      paintedBodyBg,
      // Diagnostics for the flash/divergence class of bug: which theme name
      // each subsystem THINKS is painted, so a mismatch between the backend
      // `activeName` and the actually-painted tokens is visible atomically.
      customThemeAttr,
      themeModeAttr,
      legacyThemeAttr,
      bootCacheBg: (() => {
        try {
          const raw = localStorage.getItem("cortex.theme.tokens.v1");
          if (!raw) return "(absent)";
          const t = JSON.parse(raw);
          return typeof t?.bg === "string" ? t.bg : "(no-bg)";
        } catch {
          return "(unparseable)";
        }
      })(),
      legacyThemeKey: (() => {
        try {
          return localStorage.getItem("cortex.theme") ?? "(absent)";
        } catch {
          return "(unavailable)";
        }
      })(),
    },
    // App-level state
    app: {
      hasApiKey: store.hasApiKey,
      activityTab: store.activityTab ?? null,
      currentMode: store.currentMode ?? null,
      // Explains an empty modelStripPills: compact mode hides the strip.
      statusBarCompact: store.statusBarCompact === true,
    },
    gateway,
    // Live feature-flow exercises (real commands, real events) — see
    // exerciseModelsChangedFlow above.
    flows: {
      modelsChanged: { ...modelsChangedFlow },
      jobStore: { ...jobStoreFlow },
      evalJobStore: { ...evalJobStoreFlow },
      keepAlive: { ...keepAliveFlow },
      cloneConnect: { ...cloneConnectFlow },
      routines: { ...routinesFlow },
      inlineAssist: { ...inlineAssistFlow },
      teamRun: { ...teamRunFlow },
      teamCodeLane: { ...teamCodeLaneFlow },
      focusChain: { ...focusChainFlow },
      lanes: { ...lanesFlow },
      multibufferPick: { ...multibufferPickFlow },
      researchGate: { ...researchGateFlow },
      duckGate: { ...duckGateFlow },
      gitHistory: { ...gitHistoryFlow },
      helpReference: { ...helpReferenceFlow },
      atVocab: { ...atVocabFlow },
    },
    errors: errorLog.slice(),
  };
}

async function writeOnce(): Promise<void> {
  try {
    const snapshot = await collectSnapshot();
    await invoke("e2e_write_snapshot", { payload: snapshot });
  } catch {
    // If the backend command is unavailable we simply produce no snapshot,
    // which the runner already treats as failure. Never throw into render.
  }
}

/**
 * Arm the E2E probe when launched with `CORTEX_E2E=1`. No-op otherwise. Also
 * exposes `window.__cortexE2E.snapshot()` so a future WebDriver path (or manual
 * devtools poke) can force a write on demand.
 */
export function useE2EProbe(): void {
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;

    installErrorHooks();
    (window as unknown as { __cortexE2E?: unknown }).__cortexE2E = {
      snapshot: () => collectSnapshot(),
      write: () => writeOnce(),
    };

    invoke<{ enabled: boolean }>("e2e_config")
      .then((cfg) => {
        if (cancelled || !cfg?.enabled) return;
        // Write immediately (proves first paint happened), then keep a fresh
        // heartbeat so the runner can distinguish "alive now" from "wrote once
        // then the web process died".
        void writeOnce();
        timer = setInterval(() => void writeOnce(), POLL_MS);
        // Exercise the real pull→models:changed chain, then the global
        // job-store flow (sequenced — both pull the same model and the
        // double-pull guard would reject a concurrent second pull), then the
        // eval-side job-store flow (slice 2: research/eval migration).
        // Results land in later heartbeats (the runner waits for `settled`).
        void exerciseModelsChangedFlow()
          .then(() => exerciseJobStoreFlow())
          .then(() => exerciseEvalJobStoreFlow())
          .then(() => exerciseKeepAliveFlow())
          .then(() => exerciseCloneConnectFlow())
          .then(() => exerciseRoutinesFlow())
          .then(() => exerciseInlineAssistFlow())
          .then(() => exerciseTeamRunFlow())
          .then(() => exerciseTeamCodeLaneFlow())
          .then(() => exerciseFocusChainFlow())
          .then(() => exerciseLanesFlow())
          .then(() => exerciseMultibufferPickFlow())
          .then(() => exerciseResearchGateFlow())
          .then(() => exerciseDuckGateFlow())
          .then(() => exerciseGitHistoryFlow())
          .then(() => exerciseHelpReferenceFlow())
          .then(() => exerciseAtVocabReferenceFlow());
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
      delete (window as unknown as { __cortexE2E?: unknown }).__cortexE2E;
      uninstallErrorHooks();
    };
  }, []);
}
