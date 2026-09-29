// App-wide state: boot/pairing, connection status, capabilities, the active
// project, theme, toasts, the inbox badge and the offline outbox.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import * as api from "./api";
import { demo } from "./demo";
import { errorMessage, isApiError } from "./http";
import { haptic, prefGet, prefSet } from "./native";
import { deepLinkToPath, navigate } from "./nav";
import { persistStorage, registerSw, setAppBadge } from "./push";
import { session, type ServerInfo } from "./session";
import type {
  ApiMode,
  Attachment,
  Capabilities,
  Project,
  StreamEvent,
} from "./types";
import { bus, type WsStatus } from "./ws";

export type Boot = "loading" | "pair" | "unreachable" | "ready";
export type Connection = "connected" | "reconnecting" | "offline";
export type Theme = "system" | "dark" | "light";

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error" | "success";
}

export interface OutboxItem {
  local_id: string;
  thread_id: string;
  content: string;
  model?: string;
  attachments?: Attachment[];
  project_root?: string;
  queued_ms: number;
}

interface Store {
  boot: Boot;
  bootError: string | null;
  mode: ApiMode;
  caps: Capabilities | null;
  server: ServerInfo;
  connection: Connection;
  wsStatus: WsStatus;
  /** Bumps every time the WS (re)opens after having been open — views refetch. */
  resyncNonce: number;
  demo: boolean;

  activeProjectRoot: string | null;
  setActiveProjectRoot: (root: string | null) => void;
  projects: Project[];
  refreshProjects: () => Promise<void>;

  theme: Theme;
  setTheme: (t: Theme) => void;

  toasts: Toast[];
  toast: (text: string, kind?: Toast["kind"]) => void;
  dismissToast: (id: number) => void;

  inboxCount: number;
  refreshInbox: () => void;

  outbox: OutboxItem[];
  enqueue: (item: OutboxItem) => void;
  dequeue: (local_id: string) => void;

  /** Re-probe the server (after pairing / retry). */
  reprobe: () => Promise<void>;
  completePairing: (info: ServerInfo, token: string) => Promise<void>;
  startDemo: () => Promise<void>;
  signOut: () => Promise<void>;
}

const Ctx = createContext<Store | null>(null);

const K_PROJECT = "cortex.activeProjectRoot";
const K_THEME = "cortex.theme";
const K_OUTBOX = "cortex.outbox";

export function applyTheme(t: Theme) {
  const root = document.documentElement;
  if (t === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", t);
  const dark =
    t === "dark" ||
    (t === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document
    .querySelector('meta[name="theme-color"]:not([media])')
    ?.setAttribute("content", dark ? "#0a0a0c" : "#f7f7f5");
}

export function StoreProvider({ children }: { children: ReactNode }) {
  const [boot, setBoot] = useState<Boot>("loading");
  const [bootError, setBootError] = useState<string | null>(null);
  const [mode, setModeState] = useState<ApiMode>("v2");
  const [caps, setCaps] = useState<Capabilities | null>(null);
  const [server, setServer] = useState<ServerInfo>(session.server());
  const [wsStatus, setWsStatus] = useState<WsStatus>(bus.getStatus());
  const [health, setHealth] = useState<"unknown" | "online" | "offline">(
    "unknown",
  );
  const [resyncNonce, setResync] = useState(0);
  const [activeProjectRoot, setProjRoot] = useState<string | null>(() =>
    prefGet<string | null>(K_PROJECT, null),
  );
  const [projects, setProjects] = useState<Project[]>([]);
  const [theme, setThemeState] = useState<Theme>(() =>
    prefGet<Theme>(K_THEME, "system"),
  );
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [inboxCount, setInboxCount] = useState(0);
  const [outbox, setOutbox] = useState<OutboxItem[]>(() =>
    prefGet<OutboxItem[]>(K_OUTBOX, []),
  );
  const toastSeq = useRef(0);
  const flushing = useRef(false);

  // ── Toasts ──
  const toast = useCallback((text: string, kind: Toast["kind"] = "info") => {
    const id = ++toastSeq.current;
    setToasts((t) => [...t.slice(-2), { id, text, kind }]);
    if (kind === "error") haptic("error");
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 4200);
  }, []);
  const dismissToast = useCallback(
    (id: number) => setToasts((t) => t.filter((x) => x.id !== id)),
    [],
  );

  // ── Theme ──
  useEffect(() => {
    applyTheme(theme);
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const on = () => applyTheme(theme);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, [theme]);
  const setTheme = useCallback((t: Theme) => {
    setThemeState(t);
    prefSet(K_THEME, t);
  }, []);

  // ── Projects ──
  const refreshProjects = useCallback(async () => {
    try {
      setProjects(await api.listProjects());
    } catch {
      /* views show their own errors */
    }
  }, []);
  const setActiveProjectRoot = useCallback((root: string | null) => {
    setProjRoot(root);
    prefSet(K_PROJECT, root);
    haptic("selection");
  }, []);

  // ── Inbox badge ──
  const refreshInbox = useCallback(() => {
    api
      .listApprovals()
      .then((a) => {
        setInboxCount(a.length);
        setAppBadge(a.length);
      })
      .catch(() => {});
  }, []);

  // ── Boot / probe ──
  const probe = useCallback(async () => {
    setBootError(null);
    if (session.needsServerUrl() && !demo.active) {
      setBoot("pair");
      return;
    }
    try {
      const c = await api.getCapabilities();
      if (c) {
        api.setMode("v2", c);
        setModeState("v2");
        setCaps(c);
      } else {
        // No v2 on this server: legacy PWA endpoints only.
        const h = await api.getHealth();
        api.setMode("legacy", null);
        setModeState("legacy");
        setCaps({
          server_version: h.version,
          features: [],
          local_agents: [],
          gateway: false,
        });
      }
      setHealth("online");
      setBoot("ready");
      bus.kick();
    } catch (e) {
      if (isApiError(e, "unauthorized")) {
        setBoot("pair");
        return;
      }
      setBootError(errorMessage(e));
      setBoot(
        session.token() || !session.needsServerUrl() ? "unreachable" : "pair",
      );
    }
  }, []);

  useEffect(() => {
    let alive = true;
    (async () => {
      await session.load();
      if (!alive) return;
      setServer(session.server());
      await probe();
      registerSw();
    })();
    return () => {
      alive = false;
    };
  }, [probe]);

  // 401 anywhere → back to pairing (token revoked on the desktop).
  useEffect(
    () =>
      session.onUnauthorized(() => {
        if (demo.active) return;
        setBoot("pair");
        bus.disconnect();
      }),
    [],
  );

  // ── Connection tracking ──
  useEffect(() => bus.onStatus(setWsStatus), []);
  useEffect(() => {
    if (boot !== "ready") return;
    let alive = true;
    const check = () =>
      api
        .getHealth()
        .then((h) => alive && setHealth(h.ok ? "online" : "offline"))
        .catch(() => alive && setHealth("offline"));
    const id = setInterval(check, 20_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [boot]);

  // Reconnect → resync open views; also refresh badge + projects.
  const wasOpen = useRef(false);
  useEffect(() => {
    if (wsStatus === "open") {
      if (wasOpen.current) setResync((n) => n + 1);
      wasOpen.current = true;
      setHealth("online");
      refreshInbox();
    }
  }, [wsStatus, refreshInbox]);

  // Background / foreground: pause reconnect churn while hidden, reconnect
  // in place and resync when visible again.
  useEffect(() => {
    const onVis = () => {
      if (document.visibilityState === "visible") {
        if (boot === "ready") {
          bus.kick();
          setResync((n) => n + 1);
          refreshInbox();
        } else if (boot === "unreachable") {
          probe();
        }
      }
    };
    const onOnline = () => boot === "ready" && bus.kick();
    document.addEventListener("visibilitychange", onVis);
    window.addEventListener("online", onOnline);
    window.addEventListener("cortex:resume", onVis);
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      window.removeEventListener("online", onOnline);
      window.removeEventListener("cortex:resume", onVis);
    };
  }, [boot, probe, refreshInbox]);

  // Ready → load projects + badge, keep badge fresh on approval traffic.
  useEffect(() => {
    if (boot !== "ready") return;
    refreshProjects();
    refreshInbox();
    const poll = setInterval(refreshInbox, 30_000);
    const unsub = bus.subscribe((ev: StreamEvent) => {
      if (ev.type === "approval_request") {
        haptic("warning");
        setInboxCount((n) => {
          setAppBadge(n + 1);
          return n + 1;
        });
        refreshInbox();
      } else if (ev.type === "approval_resolved") {
        setInboxCount((n) => {
          setAppBadge(Math.max(0, n - 1));
          return Math.max(0, n - 1);
        });
        refreshInbox();
      }
    });
    return () => {
      clearInterval(poll);
      unsub();
    };
  }, [boot, refreshProjects, refreshInbox]);

  // ── Deep links: cortex://…, hash on load handled by the router itself. ──
  useEffect(() => {
    const on = (e: Event) => {
      const link = (e as CustomEvent<string>).detail;
      const path = typeof link === "string" ? deepLinkToPath(link) : null;
      if (path) navigate(path);
    };
    window.addEventListener("cortex:deeplink", on);
    return () => window.removeEventListener("cortex:deeplink", on);
  }, []);

  // ── Outbox ──
  const enqueue = useCallback(
    (item: OutboxItem) =>
      setOutbox((o) => {
        const n = [...o, item];
        prefSet(K_OUTBOX, n);
        return n;
      }),
    [],
  );
  const dequeue = useCallback(
    (local_id: string) =>
      setOutbox((o) => {
        const n = o.filter((x) => x.local_id !== local_id);
        prefSet(K_OUTBOX, n);
        return n;
      }),
    [],
  );
  const connection: Connection =
    health === "offline"
      ? "offline"
      : wsStatus === "open"
        ? "connected"
        : "reconnecting";

  useEffect(() => {
    if (connection !== "connected" || outbox.length === 0 || flushing.current) {
      return;
    }
    flushing.current = true;
    (async () => {
      for (const item of outbox) {
        try {
          const r = await api.sendMessage(item.thread_id, {
            content: item.content,
            model: item.model,
            attachments: item.attachments,
            project_root: item.project_root,
          });
          dequeue(item.local_id);
          window.dispatchEvent(
            new CustomEvent("cortex:sent", {
              detail: { ...r, local_id: item.local_id },
            }),
          );
        } catch (e) {
          if (isApiError(e, "network") || isApiError(e, "timeout")) break;
          dequeue(item.local_id);
          toast(`Couldn't send a queued message: ${errorMessage(e)}`, "error");
        }
      }
      flushing.current = false;
    })();
  }, [connection, outbox, dequeue, toast]);

  // ── Pairing / sign-out / demo ──
  const completePairing = useCallback(
    async (info: ServerInfo, token: string) => {
      await session.setServer(info, token);
      setServer(session.server());
      persistStorage();
      setBoot("loading");
      await probe();
    },
    [probe],
  );

  const signOut = useCallback(async () => {
    bus.disconnect();
    const dev = session.server().device_id;
    if (demo.active) demo.stop();
    else if (dev) await api.revokeDevice(dev).catch(() => {});
    await session.clear();
    setServer(session.server());
    setCaps(null);
    setInboxCount(0);
    setAppBadge(0);
    setBoot("pair");
    navigate("/chats", { replace: true });
  }, []);

  const startDemo = useCallback(async () => {
    demo.start();
    await session.setServer(
      {
        url: "",
        device_id: "dev-this",
        server_name: "Demo Cortex",
        server_version: "demo",
      },
      "demo",
    );
    setServer(session.server());
    setBoot("loading");
    await probe();
  }, [probe]);

  const value = useMemo<Store>(
    () => ({
      boot,
      bootError,
      mode,
      caps,
      server,
      connection,
      wsStatus,
      resyncNonce,
      demo: demo.active,
      activeProjectRoot,
      setActiveProjectRoot,
      projects,
      refreshProjects,
      theme,
      setTheme,
      toasts,
      toast,
      dismissToast,
      inboxCount,
      refreshInbox,
      outbox,
      enqueue,
      dequeue,
      reprobe: probe,
      completePairing,
      startDemo,
      signOut,
    }),
    [
      boot,
      bootError,
      mode,
      caps,
      server,
      connection,
      wsStatus,
      resyncNonce,
      activeProjectRoot,
      setActiveProjectRoot,
      projects,
      refreshProjects,
      theme,
      setTheme,
      toasts,
      toast,
      dismissToast,
      inboxCount,
      refreshInbox,
      outbox,
      enqueue,
      dequeue,
      probe,
      completePairing,
      startDemo,
      signOut,
    ],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useStore(): Store {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useStore must be used within StoreProvider");
  return ctx;
}
