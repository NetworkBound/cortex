// A single shared WebSocket for the whole app. Views subscribe and filter
// frames by `run_id`. Auto-reconnects with backoff. In dev, vite proxies `/ws`
// to VITE_API_BASE so the same-origin URL works there too.

import type { WsFrameBase } from "./types";

type Listener = (frame: WsFrameBase) => void;
type StatusListener = (status: WsStatus) => void;

export type WsStatus = "connecting" | "open" | "closed";

class SharedWs {
  private ws: WebSocket | null = null;
  private listeners = new Set<Listener>();
  private statusListeners = new Set<StatusListener>();
  private status: WsStatus = "closed";
  private backoff = 500;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private stopped = false;

  private url(): string {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    return `${proto}://${location.host}/ws`;
  }

  connect() {
    this.stopped = false;
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }
    this.setStatus("connecting");
    try {
      this.ws = new WebSocket(this.url());
    } catch {
      this.scheduleReconnect();
      return;
    }

    this.ws.onopen = () => {
      this.backoff = 500;
      this.setStatus("open");
    };
    this.ws.onmessage = (ev) => {
      let frame: WsFrameBase;
      try {
        frame = JSON.parse(ev.data as string) as WsFrameBase;
      } catch {
        return;
      }
      for (const l of this.listeners) {
        try {
          l(frame);
        } catch {
          /* a bad listener shouldn't kill the bus */
        }
      }
    };
    this.ws.onclose = () => {
      this.setStatus("closed");
      this.ws = null;
      if (!this.stopped) this.scheduleReconnect();
    };
    this.ws.onerror = () => {
      // onclose follows; reconnect handled there.
      this.ws?.close();
    };
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    const delay = this.backoff;
    this.backoff = Math.min(this.backoff * 2, 10_000);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private setStatus(s: WsStatus) {
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
