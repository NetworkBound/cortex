// One shared WebSocket for the whole app.
//
// - Authenticates with `?token=` (and an `auth` frame for servers that want
//   it), then subscribes to every thread.
// - Reconnects with capped exponential backoff, resubscribes, and tells
//   listeners it (re)opened so they can resync (refetch the open thread).
// - Backgrounding pauses the reconnect loop; foregrounding kicks it.
// - Both wire dialects (contract `token`/`done`/… and legacy `chat_*`) are
//   normalised into `StreamEvent` so views see one vocabulary.

import { wsUrl } from "./http";
import { session } from "./session";
import { demo } from "./demo";
import type { Approval, StreamEvent, Thread, ToolCall, WsFrame } from "./types";

export type WsStatus = "connecting" | "open" | "closed";

type Listener = (ev: StreamEvent) => void;
type StatusListener = (status: WsStatus) => void;

const PING_IDLE_MS = 45_000;

class SharedWs {
  private ws: WebSocket | null = null;
  private listeners = new Set<Listener>();
  private statusListeners = new Set<StatusListener>();
  private status: WsStatus = "closed";
  private backoff = 500;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private idle: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  private hadOpen = false;
  private legacyToolSeq = 0;

  connect() {
    this.stopped = false;
    if (demo.active) {
      this.setStatus("open");
      this.hadOpen = true;
      return;
    }
    if (
      this.ws &&
      (this.ws.readyState === WebSocket.OPEN ||
        this.ws.readyState === WebSocket.CONNECTING)
    ) {
      return;
    }
    this.setStatus("connecting");
    let ws: WebSocket;
    try {
      ws = new WebSocket(wsUrl());
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.backoff = 500;
      const token = session.token();
      if (token) this.send({ type: "auth", token });
      this.send({ type: "subscribe", threads: ["*"] });
      this.armIdle();
      this.setStatus("open");
      this.hadOpen = true;
    };
    ws.onmessage = (ev) => {
      if (this.ws !== ws) return;
      this.armIdle();
      let frame: WsFrame;
      try {
        frame = JSON.parse(String(ev.data)) as WsFrame;
      } catch {
        return;
      }
      const norm = normalise(frame, () => `legacy-${++this.legacyToolSeq}`);
      if (norm.type === "ping") {
        this.send({ type: "pong" });
        return;
      }
      for (const l of this.listeners) {
        try {
          l(norm);
        } catch {
          /* a bad listener shouldn't kill the bus */
        }
      }
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.clearIdle();
      this.setStatus("closed");
      if (!this.stopped) this.scheduleReconnect();
    };
    ws.onerror = () => {
      ws.close();
    };
  }

  /** Drop the connection and stop reconnecting (sign-out / background). */
  disconnect() {
    this.stopped = true;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.clearIdle();
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      ws.onclose = null;
      ws.close();
    }
    this.setStatus("closed");
  }

  /** Foreground: reconnect right away instead of waiting for the backoff. */
  kick() {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.backoff = 500;
    this.connect();
  }

  /** True if the socket has been open at least once this app lifetime and
   *  therefore a later `open` is a *re*connect that warrants a resync. */
  reconnected(): boolean {
    return this.hadOpen;
  }

  /** Push a normalised event to every listener (demo replay, tests). */
  inject(ev: StreamEvent) {
    for (const l of this.listeners) {
      try {
        l(ev);
      } catch {
        /* ignore */
      }
    }
  }

  send(obj: unknown) {
    try {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify(obj));
      }
    } catch {
      /* ignore */
    }
  }

  private armIdle() {
    this.clearIdle();
    // The server pings; if nothing at all arrives for a long while the link
    // is probably half-dead (phone slept) — recycle it.
    this.idle = setTimeout(() => {
      this.ws?.close();
    }, PING_IDLE_MS * 2);
  }

  private clearIdle() {
    if (this.idle) {
      clearTimeout(this.idle);
      this.idle = null;
    }
  }

  private scheduleReconnect() {
    if (this.timer || this.stopped) return;
    const delay = this.backoff + Math.floor(Math.random() * 250);
    this.backoff = Math.min(this.backoff * 2, 15_000);
    this.timer = setTimeout(() => {
      this.timer = null;
      this.connect();
    }, delay);
  }

  private setStatus(s: WsStatus) {
    if (s === this.status) return;
    this.status = s;
    for (const l of this.statusListeners) l(s);
  }

  getStatus(): WsStatus {
    return this.status;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  onStatus(fn: StatusListener): () => void {
    this.statusListeners.add(fn);
    fn(this.status);
    return () => this.statusListeners.delete(fn);
  }
}

export const bus = new SharedWs();
demo.sink = (ev) => bus.inject(ev);

// ── Frame normalisation ────────────────────────────────────────────────────

const str = (v: unknown): string | undefined =>
  typeof v === "string" ? v : undefined;

function normalise(f: WsFrame, legacyId: () => string): StreamEvent {
  const run_id = str(f.run_id) ?? "";
  const thread_id = str(f.thread_id);
  switch (f.type) {
    // ── contract (v2) ──
    case "token":
      return { type: "token", run_id, thread_id, delta: str(f.delta) ?? "" };
    case "reasoning":
      return {
        type: "reasoning",
        run_id,
        thread_id,
        delta: str(f.delta) ?? str(f.text) ?? "",
      };
    case "tool_call": {
      const t = (f.tool ?? {}) as Partial<ToolCall>;
      return {
        type: "tool_call",
        run_id,
        thread_id,
        tool: {
          id: t.id ?? legacyId(),
          name: t.name ?? "tool",
          args_preview: t.args_preview,
          status: t.status ?? "pending",
          result_preview: t.result_preview,
        },
      };
    }
    case "tool_result": {
      const t = (f.tool ?? {}) as Partial<ToolCall>;
      return { type: "tool_result", run_id, thread_id, tool: t };
    }
    case "approval_request":
      return {
        type: "approval_request",
        run_id,
        thread_id,
        approval: (f.approval ?? {}) as Approval,
      };
    case "approval_resolved":
      return {
        type: "approval_resolved",
        run_id: run_id || undefined,
        thread_id,
        approval_id: str(f.approval_id),
        decision: str(f.decision) ?? "",
      };
    case "done":
      return {
        type: "done",
        run_id,
        thread_id,
        usage: (f.usage as StreamEvent & { usage?: never })
          ? (f.usage as never)
          : undefined,
      };
    case "error":
      return {
        type: "error",
        run_id,
        thread_id,
        message: str(f.message) ?? "Run failed",
      };
    case "thread_updated":
      return { type: "thread_updated", thread: f.thread as Thread };
    case "ping":
      return { type: "ping" };

    // ── legacy (`MobileEvent`) ──
    case "chat_token":
      return { type: "token", run_id, delta: str(f.delta) ?? "" };
    case "chat_reasoning":
      return { type: "reasoning", run_id, delta: str(f.text) ?? "" };
    case "chat_tool_call":
      return {
        type: "tool_call",
        run_id,
        tool: {
          id: legacyId(),
          name: str(f.name) ?? "tool",
          args_preview: str(f.preview),
          status: "pending",
        },
      };
    case "chat_tool_result":
      return {
        type: "tool_result",
        run_id,
        tool: {
          name: str(f.name),
          status: f.ok === false ? "error" : "done",
          result_preview: str(f.summary),
        },
      };
    case "chat_file_edit":
      return {
        type: "tool_call",
        run_id,
        tool: {
          id: legacyId(),
          name: "edit",
          args_preview: `${str(f.path) ?? ""} (${String(f.lines_changed ?? "?")} lines)`,
          status: "done",
        },
      };
    case "chat_approval":
      return {
        type: "approval_request",
        run_id,
        approval: {
          id: "", // legacy frames carry no id; the inbox list has it
          run_id,
          tool: str(f.tool),
          detail: str(f.preview),
          choices: Array.isArray(f.choices) ? (f.choices as string[]) : [],
        },
      };
    case "chat_approval_resolved":
      return {
        type: "approval_resolved",
        run_id,
        decision: str(f.choice) ?? "",
      };
    case "chat_done":
      return {
        type: "done",
        run_id,
        usage:
          typeof f.total_tokens === "number"
            ? { total_tokens: f.total_tokens }
            : undefined,
      };
    case "chat_error":
      return { type: "error", run_id, message: str(f.message) ?? "Run failed" };
    default:
      return { type: "other", frame: f };
  }
}
