// "Try the demo": an in-memory Cortex that answers the same `request()` calls
// and streams the same `StreamEvent`s the real server would, so every screen
// works without a desktop (App Store review, screenshots, first-run tour).
// Intentionally tiny — enough canned data to make each tab non-empty.

import type {
  Approval,
  Message,
  StreamEvent,
  Project,
  Routine,
  Run,
  Thread,
  ToolCall,
} from "./types";

const now = Date.now();
const H = 3_600_000;

const DEMO_KEY = "cortex.demo";

class Demo {
  active = false;
  /** Installed by ws.ts: pushes replayed events into the shared bus. */
  sink: ((ev: StreamEvent) => void) | null = null;
  private threads: Thread[] = [];
  private messages = new Map<string, Message[]>();
  private approvals: Approval[] = [];
  private runs: Run[] = [];
  private routines: Routine[] = [];
  private projects: Project[] = [];
  private timers: ReturnType<typeof setTimeout>[] = [];
  private seq = 0;

  constructor() {
    try {
      this.active = localStorage.getItem(DEMO_KEY) === "1";
    } catch {
      this.active = false;
    }
    if (this.active) this.seed();
  }

  start() {
    this.active = true;
    try {
      localStorage.setItem(DEMO_KEY, "1");
    } catch {
      /* ignore */
    }
    this.seed();
  }

  stop() {
    this.active = false;
    for (const t of this.timers) clearTimeout(t);
    this.timers = [];
    try {
      localStorage.removeItem(DEMO_KEY);
    } catch {
      /* ignore */
    }
  }

  private emit(ev: StreamEvent) {
    this.sink?.(ev);
  }

  private id(p = "d") {
    return `${p}${++this.seq}`;
  }

  private seed() {
    this.seq = 0;
    this.projects = [
      {
        root: "/home/you/code/cortex",
        name: "cortex",
        trusted: true,
        branch: "main",
        dirty_files: 3,
        last_opened_ms: now - H,
      },
      {
        root: "/home/you/code/website",
        name: "website",
        trusted: true,
        branch: "feat/pricing",
        dirty_files: 0,
        last_opened_ms: now - 30 * H,
      },
    ];
    const t1: Thread = {
      id: "t1",
      title: "Fix flaky login test",
      project_root: this.projects[0].root,
      agent_id: "claude-cli",
      model: "claude-sonnet-4-6",
      created_ms: now - 2 * H,
      last_ms: now - 5 * 60_000,
      pending_approvals: 1,
      running: true,
      last_preview: "I'll run the test suite to confirm the fix…",
    };
    const t2: Thread = {
      id: "t2",
      title: "Pricing page copy",
      project_root: this.projects[1].root,
      agent_id: "gateway",
      model: "gpt-5",
      created_ms: now - 26 * H,
      last_ms: now - 25 * H,
      pending_approvals: 0,
      running: false,
      last_preview: "Here are three variants of the hero headline.",
    };
    const t3: Thread = {
      id: "t3",
      title: "Explain the WS reconnect logic",
      project_root: this.projects[0].root,
      agent_id: "claude-cli",
      model: "claude-sonnet-4-6",
      created_ms: now - 3 * 24 * H,
      last_ms: now - 3 * 24 * H,
      pending_approvals: 0,
      running: false,
      last_preview: "The bus keeps one socket and backs off exponentially.",
    };
    this.threads = [t1, t2, t3];
    const approval: Approval = {
      id: "a1",
      tool: "shell",
      detail: "pnpm test -- --filter login",
      resolved: false,
      risk: "exec",
      run_id: "r1",
      thread_id: "t1",
    };
    this.approvals = [approval];
    this.messages.set("t1", [
      {
        id: "m1",
        role: "user",
        content: "The login e2e test is flaky on CI. Can you find out why?",
        ts_ms: now - 2 * H,
      },
      {
        id: "m2",
        role: "assistant",
        ts_ms: now - 2 * H + 20_000,
        run_id: "r0",
        routing_reason: "local claude-cli · repo task",
        content:
          "Looking at `login.spec.ts`, the test clicks **Sign in** before the form's hydration finishes. I've added an explicit `await page.waitForSelector('[data-hydrated]')`.\n\n```ts\nawait page.waitForSelector('[data-hydrated]');\nawait page.click('text=Sign in');\n```",
        reasoning:
          "The failure only reproduces under load, which points at a race rather than a logic bug.",
        tool_calls: [
          {
            id: "tc1",
            name: "read_file",
            args_preview: "e2e/login.spec.ts",
            status: "done",
            result_preview: "42 lines",
            duration_ms: 120,
          },
          {
            id: "tc2",
            name: "edit",
            args_preview: "e2e/login.spec.ts (+2 −0)",
            status: "done",
            duration_ms: 340,
          },
        ],
        usage: { input_tokens: 4210, output_tokens: 612, cost_usd: 0.021 },
      },
      {
        id: "m3",
        role: "user",
        content: "Great — run the test to confirm.",
        ts_ms: now - 6 * 60_000,
      },
      {
        id: "m4",
        role: "assistant",
        content: "I'll run the test suite to confirm the fix…",
        ts_ms: now - 5 * 60_000,
        run_id: "r1",
        tool_calls: [
          {
            id: "tc3",
            name: "shell",
            args_preview: "pnpm test -- --filter login",
            status: "pending",
          },
        ],
        approval,
      },
    ]);
    this.messages.set("t2", [
      {
        id: "m5",
        role: "user",
        content: "Give me three hero headlines for the pricing page.",
        ts_ms: now - 26 * H,
      },
      {
        id: "m6",
        role: "assistant",
        content:
          "Here are three variants of the hero headline.\n\n1. **Pay for outcomes, not seats.**\n2. **One price. Every model.**\n3. **Your agents, your rules, your budget.**",
        ts_ms: now - 25 * H,
        usage: { input_tokens: 800, output_tokens: 90, cost_usd: 0.004 },
      },
    ]);
    this.messages.set("t3", [
      {
        id: "m7",
        role: "user",
        content: "Explain the WS reconnect logic in mobile/src/lib/ws.ts.",
        ts_ms: now - 3 * 24 * H,
      },
      {
        id: "m8",
        role: "assistant",
        content:
          "The bus keeps one socket and backs off exponentially (500 ms → 15 s) with jitter, resubscribes on open, and recycles a half-dead link when nothing arrives for 90 s.",
        ts_ms: now - 3 * 24 * H + 9_000,
        error: undefined,
      },
    ]);
    this.runs = [
      {
        run_id: "r1",
        thread_id: "t1",
        started_ms: now - 5 * 60_000,
        status: "running",
        agent_id: "claude-cli",
        model: "claude-sonnet-4-6",
      },
      {
        run_id: "r0",
        thread_id: "t1",
        started_ms: now - 2 * H,
        ended_ms: now - 2 * H + 41_000,
        status: "done",
        agent_id: "claude-cli",
        model: "claude-sonnet-4-6",
        cost_usd: 0.021,
        tokens: 4822,
      },
      {
        run_id: "r2",
        thread_id: "t2",
        started_ms: now - 25 * H,
        ended_ms: now - 25 * H + 6_000,
        status: "done",
        agent_id: "gateway",
        model: "gpt-5",
        cost_usd: 0.004,
        tokens: 890,
      },
      {
        run_id: "r3",
        thread_id: "t3",
        started_ms: now - 40 * H,
        ended_ms: now - 40 * H + 2_000,
        status: "error",
        agent_id: "gateway",
        model: "gpt-5",
      },
    ];
    this.routines = [
      {
        id: "ro1",
        name: "Morning triage",
        prompt: "Summarise open issues and failing CI runs.",
        interval_minutes: 0,
        daily_at: "08:30",
        enabled: true,
        agent_id: "claude-cli",
        project_root: this.projects[0].root,
        last_run_unix_ms: now - 20 * H,
        last_status: "ok",
        next_run_unix_ms: now + 4 * H,
      },
      {
        id: "ro2",
        name: "Dependency audit",
        prompt: "Run pnpm audit and report anything high or critical.",
        interval_minutes: 1440,
        enabled: false,
        last_status: "",
        next_run_unix_ms: null,
      },
    ];
  }

  private later(ms: number, fn: () => void) {
    this.timers.push(setTimeout(fn, ms));
  }

  async request<T>(method: string, path: string, body: unknown): Promise<T> {
    await new Promise((r) => setTimeout(r, 120));
    const [p, qs] = path.split("?");
    const params = new URLSearchParams(qs ?? "");
    const b = (body ?? {}) as Record<string, unknown>;
    const seg = p.split("/").filter(Boolean); // ["api","v2",...]
    const v2 = seg[1] === "v2" ? seg.slice(2) : null;
    const j = (v: unknown) => v as T;

    if (p === "/api/health") return j({ ok: true, version: "demo" });
    if (!v2) {
      if (p === "/api/approvals") return j(this.approvals);
      throw new Error(`demo: ${method} ${path} not modelled`);
    }
    const [a, b1, c] = v2;
    switch (a) {
      case "capabilities":
        return j({
          server_version: "demo",
          features: [
            "threads",
            "replay",
            "routines",
            "git",
            "checkpoints",
            "reliability",
            "usage",
            "projects.add",
            "push.web",
          ],
          local_agents: ["claude-cli", "codex-cli"],
          gateway: true,
        });
      case "threads": {
        if (!b1 && method === "GET") {
          const proj = params.get("project");
          return j({
            threads: this.threads.filter(
              (t) => !proj || t.project_root === proj,
            ),
            next_cursor: null,
          });
        }
        if (!b1 && method === "POST") {
          const t: Thread = {
            id: this.id("t"),
            title: (b.title as string) || "New chat",
            project_root: (b.project_root as string) ?? null,
            created_ms: Date.now(),
            last_ms: Date.now(),
            pending_approvals: 0,
            running: false,
            last_preview: "",
          };
          this.threads.unshift(t);
          this.messages.set(t.id, []);
          return j(t);
        }
        const t = this.threads.find((x) => x.id === b1);
        if (!t) throw new Error("not found");
        if (c === "messages")
          return j({ messages: this.messages.get(t.id) ?? [] });
        if (c === "send") return j({ run_id: this.streamReply(t, b) });
        if (method === "PATCH") {
          t.title = (b.title as string) || t.title;
          return j(t);
        }
        if (method === "DELETE") {
          this.threads = this.threads.filter((x) => x.id !== t.id);
          return j(undefined);
        }
        return j(t);
      }
      case "runs":
        if (!b1) return j({ runs: this.runs });
        if (c === "stop") {
          const r = this.runs.find((x) => x.run_id === b1);
          if (r && r.status === "running") {
            r.status = "stopped";
            r.ended_ms = Date.now();
            this.emit({
              type: "done",
              run_id: r.run_id,
              thread_id: r.thread_id,
            });
          }
          return j(undefined);
        }
        if (c === "timeline") {
          const r = this.runs.find((x) => x.run_id === b1);
          const s = r?.started_ms ?? now;
          return j({
            events: [
              { ts_ms: s, kind: "prompt", summary: "Run the test to confirm." },
              {
                ts_ms: s + 300,
                kind: "route",
                summary: "claude-cli · repo task",
              },
              {
                ts_ms: s + 1_200,
                kind: "tool_call",
                summary: "shell: pnpm test -- --filter login",
                detail: "cwd: /home/you/code/cortex",
              },
              {
                ts_ms: s + 1_300,
                kind: "approval",
                summary: "exec approval requested",
              },
              ...(r?.status === "done"
                ? [
                    {
                      ts_ms: s + 30_000,
                      kind: "tool_result",
                      summary: "12 passed",
                    },
                    {
                      ts_ms: s + 41_000,
                      kind: "result",
                      summary: "Done · 4.8k tok · $0.02",
                    },
                  ]
                : r?.status === "error"
                  ? [
                      {
                        ts_ms: s + 2_000,
                        kind: "error",
                        summary: "gateway: 429 rate limited",
                      },
                    ]
                  : []),
            ],
          });
        }
        return j({ runs: this.runs });
      case "approvals": {
        const ap = this.approvals.find((x) => x.id === b1);
        if (ap) {
          this.approvals = this.approvals.filter((x) => x.id !== b1);
          const decision = (b.decision as string) ?? "approve";
          const t = this.threads.find((x) => x.id === ap.thread_id);
          if (t) t.pending_approvals = 0;
          this.emit({
            type: "approval_resolved",
            approval_id: ap.id,
            run_id: ap.run_id,
            thread_id: ap.thread_id,
            decision,
          });
          if (t && decision === "approve") this.finishRun(t, ap.run_id ?? "r1");
          else if (t) {
            const r = this.runs.find((x) => x.run_id === ap.run_id);
            if (r) {
              r.status = "stopped";
              r.ended_ms = Date.now();
            }
            t.running = false;
            this.emit({
              type: "done",
              run_id: ap.run_id ?? "",
              thread_id: t.id,
            });
          }
        }
        return j(undefined);
      }
      case "projects":
        if (b1 === "discover")
          return j({
            projects: [{ root: "/home/you/code/blog", name: "blog" }],
          });
        if (b1 === "add") {
          const root = String(b.root ?? "");
          const pr: Project = {
            root,
            name: root.split("/").pop() || root,
            trusted: false,
          };
          this.projects.push(pr);
          return j(pr);
        }
        if (b1 === "git" && c === "status")
          return j({
            branch: "main",
            ahead: 1,
            behind: 0,
            files: [
              { path: "e2e/login.spec.ts", status: "M" },
              { path: "mobile/src/lib/ws.ts", status: "M" },
              { path: "docs/notes.md", status: "??" },
            ],
          });
        if (b1 === "git" && c === "diff")
          return j({
            diff: `diff --git a/e2e/login.spec.ts b/e2e/login.spec.ts\n--- a/e2e/login.spec.ts\n+++ b/e2e/login.spec.ts\n@@ -10,6 +10,8 @@ test("login", async ({ page }) => {\n   await page.goto("/login");\n+  // Wait for hydration before interacting (flaky on CI otherwise).\n+  await page.waitForSelector("[data-hydrated]");\n   await page.fill("#email", user.email);\n   await page.fill("#password", user.password);\n   await page.click("text=Sign in");\n`,
            truncated: false,
          });
        return j({ projects: this.projects });
      case "checkpoints":
        if (method === "POST" && !b1)
          return j({
            id: this.id("cp"),
            label: b.label,
            created_ms: Date.now(),
          });
        if (c === "restore") return j(undefined);
        return j({
          checkpoints: [
            {
              id: "cp1",
              label: "before login fix",
              created_ms: now - 2 * H,
              files: 1,
            },
          ],
        });
      case "reliability":
        return j({
          totals: {
            runs: 42,
            success_rate: 0.93,
            p50_ms: 8_200,
            p95_ms: 41_000,
            total_tokens: 210_000,
            est_usd: 1.84,
          },
          by_provider: [
            {
              key: "claude-cli",
              runs: 30,
              success_rate: 0.97,
              p50_ms: 9_000,
              p95_ms: 44_000,
              total_tokens: 160_000,
              est_usd: 1.4,
            },
            {
              key: "gateway",
              runs: 12,
              success_rate: 0.83,
              p50_ms: 4_000,
              p95_ms: 12_000,
              total_tokens: 50_000,
              est_usd: 0.44,
              top_error_class: "rate_limited",
            },
          ],
          by_model: [
            {
              key: "claude-sonnet-4-6",
              runs: 30,
              success_rate: 0.97,
              p50_ms: 9_000,
              p95_ms: 44_000,
            },
            {
              key: "gpt-5",
              runs: 12,
              success_rate: 0.83,
              p50_ms: 4_000,
              p95_ms: 12_000,
            },
          ],
        });
      case "usage":
        return j({
          claude: {
            five_hour_pct: 62,
            seven_day_pct: 31,
            resets_ms: now + 2 * H,
          },
          chatgpt: {
            five_hour_pct: 12,
            seven_day_pct: 8,
            resets_ms: now + 4 * H,
          },
          budget: { spent_usd: 18.4, cap_usd: 50 },
        });
      case "routines": {
        if (!b1 && method === "GET") return j({ routines: this.routines });
        if (!b1 && method === "POST") {
          const r = {
            ...(b as unknown as Routine),
            id: this.id("ro"),
            last_status: "",
          };
          this.routines.push(r);
          return j(r);
        }
        const r = this.routines.find((x) => x.id === b1);
        if (!r) throw new Error("not found");
        if (c === "run") {
          r.last_run_unix_ms = Date.now();
          r.last_status = "ok";
          return j(undefined);
        }
        if (c === "history")
          return j({
            history: [
              {
                ts_ms: now - 20 * H,
                status: "ok",
                output: "3 open issues, CI green.",
              },
            ],
          });
        if (method === "PATCH") {
          Object.assign(r, b);
          return j(r);
        }
        if (method === "DELETE") {
          this.routines = this.routines.filter((x) => x.id !== b1);
          return j(undefined);
        }
        return j(r);
      }
      case "models":
        return j({
          models: [
            {
              id: "claude-sonnet-4-6",
              label: "Claude Sonnet 4.6",
              provider: "anthropic",
              local: true,
              cost_tier: "mid",
            },
            {
              id: "claude-opus-4-6",
              label: "Claude Opus 4.6",
              provider: "anthropic",
              local: true,
              cost_tier: "high",
            },
            {
              id: "gpt-5",
              label: "GPT-5",
              provider: "openai",
              local: false,
              cost_tier: "mid",
            },
            {
              id: "local/qwen3",
              label: "Qwen3 (local)",
              provider: "ollama",
              local: true,
              cost_tier: "free",
            },
          ],
          default: "claude-sonnet-4-6",
        });
      case "settings":
        if (method === "PUT") return j(b);
        return j({
          default_model: "claude-sonnet-4-6",
          default_agent_id: "claude-cli",
          plan_mode: false,
          sandbox_tier: "standard",
        });
      case "push":
        if (b1 === "status")
          return j({
            provider: "ntfy",
            enabled: true,
            events: ["approval", "done", "error"],
          });
        if (b1 === "vapid") return j({ public_key: "" });
        return j(undefined);
      case "devices":
        if (method === "DELETE") return j(undefined);
        return j({
          devices: [
            {
              id: "dev-this",
              name: "This phone (demo)",
              created_ms: now - 3 * H,
              last_seen_ms: now,
              current: true,
            },
            {
              id: "dev-2",
              name: "iPad",
              created_ms: now - 60 * H,
              last_seen_ms: now - 5 * H,
            },
          ],
        });
      case "pair":
        return j({
          token: "demo",
          device_id: "dev-this",
          server_name: "Demo Cortex",
          server_version: "demo",
        });
    }
    throw new Error(`demo: ${method} ${path} not modelled`);
  }

  /** Stream a canned reply for a sent message. */
  private streamReply(t: Thread, b: Record<string, unknown>): string {
    const list = this.messages.get(t.id) ?? [];
    const runId = this.id("r");
    const userMsg: Message = {
      id: this.id("m"),
      role: "user",
      content: String(b.content ?? ""),
      ts_ms: Date.now(),
    };
    const asst: Message = {
      id: this.id("m"),
      role: "assistant",
      content: "",
      ts_ms: Date.now(),
      run_id: runId,
      tool_calls: [],
      routing_reason: "demo · canned reply",
    };
    list.push(userMsg, asst);
    this.messages.set(t.id, list);
    t.running = true;
    t.last_ms = Date.now();
    this.runs.unshift({
      run_id: runId,
      thread_id: t.id,
      started_ms: Date.now(),
      status: "running",
      agent_id: "demo",
      model: "demo",
    });
    const text =
      "Sure — here's what I'd do.\n\n1. Reproduce it locally with `pnpm test`.\n2. Read the failing spec.\n3. Patch and re-run.\n\nThis is a **demo** reply streamed through the same WebSocket path the real server uses.";
    const words = text.split(/(?<=\s)/);
    let i = 0;
    let acc = "";
    const tick = () => {
      if (!this.active) return;
      if (i < words.length) {
        const delta = words[i++];
        acc += delta;
        this.emit({ type: "token", run_id: runId, thread_id: t.id, delta });
        this.later(35, tick);
        return;
      }
      asst.content = acc;
      const tool: ToolCall = {
        id: this.id("tc"),
        name: "read_file",
        args_preview: "package.json",
        status: "pending",
      };
      asst.tool_calls = [tool];
      this.emit({ type: "tool_call", run_id: runId, thread_id: t.id, tool });
      this.later(700, () => {
        tool.status = "done";
        tool.result_preview = "64 lines";
        tool.duration_ms = 700;
        this.emit({
          type: "tool_result",
          run_id: runId,
          thread_id: t.id,
          tool: {
            id: tool.id,
            name: tool.name,
            status: "done",
            result_preview: "64 lines",
            duration_ms: 700,
          },
        });
        this.later(300, () =>
          this.finishRun(t, runId, {
            input_tokens: 1200,
            output_tokens: 140,
            cost_usd: 0.006,
          }),
        );
      });
    };
    this.later(250, () => {
      this.emit({
        type: "reasoning",
        run_id: runId,
        thread_id: t.id,
        delta: "The user wants a plan; keep it short.",
      });
      tick();
    });
    return runId;
  }

  private finishRun(t: Thread, runId: string, usage?: Message["usage"]) {
    const r = this.runs.find((x) => x.run_id === runId);
    if (r) {
      r.status = "done";
      r.ended_ms = Date.now();
      r.cost_usd = usage?.cost_usd ?? 0.02;
    }
    t.running = false;
    t.pending_approvals = 0;
    const list = this.messages.get(t.id) ?? [];
    const asst = list.find((m) => m.run_id === runId && m.role === "assistant");
    if (asst) {
      if (asst.approval) asst.approval.resolved = true;
      for (const tc of asst.tool_calls ?? [])
        if (tc.status === "pending") tc.status = "done";
      if (usage) asst.usage = usage;
      t.last_preview = asst.content.slice(0, 80);
    }
    this.emit({ type: "done", run_id: runId, thread_id: t.id, usage });
    this.emit({ type: "thread_updated", thread: { ...t } });
  }
}

export const demo = new Demo();
