// Tiny app-wide store: the active project root (shared by Chat + Ultimate) and
// the live WS connection status. Persists the active project to localStorage.

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { bus, type WsStatus } from "./ws";
import { getHealth } from "./api";
import type { Project } from "./types";
import { projectPath } from "./types";

/** Server-reachability, derived from polling GET /api/health. `unknown` until
 *  the first probe resolves. */
export type ServerHealth = "unknown" | "online" | "offline";

interface Store {
  activeProject: Project | null;
  setActiveProject: (p: Project | null) => void;
  activeProjectRoot: string | undefined;
  wsStatus: WsStatus;
  /** Whether the embedded server answers GET /api/health. Drives the
   *  offline/online indicator independently of the streaming WS. */
  serverHealth: ServerHealth;
  /** A session the Recent tab asked the Chat view to open + resume, or null.
   *  The Chat view loads its history then clears it via `setOpenSession(null)`. */
  openSession: string | null;
  setOpenSession: (id: string | null) => void;
  /** Monotonic counter; bumping it asks the Chat view to start a fresh
   *  conversation. The header "New chat" button increments it. */
  newChatNonce: number;
  requestNewChat: () => void;
}

const StoreCtx = createContext<Store | null>(null);

const LS_KEY = "cortex.activeProject";

export function StoreProvider({ children }: { children: ReactNode }) {
  const [activeProject, setActiveProjectState] = useState<Project | null>(() => {
    try {
      const raw = localStorage.getItem(LS_KEY);
      return raw ? (JSON.parse(raw) as Project) : null;
    } catch {
      return null;
    }
  });
  const [wsStatus, setWsStatus] = useState<WsStatus>(bus.getStatus());
  const [serverHealth, setServerHealth] = useState<ServerHealth>("unknown");
  const [openSession, setOpenSession] = useState<string | null>(null);
  const [newChatNonce, setNewChatNonce] = useState(0);

  useEffect(() => {
    bus.connect();
    return bus.onStatus(setWsStatus);
  }, []);

  // Poll the health endpoint so the indicator reflects actual server
  // reachability (the streaming WS can be down while the API is fine, and vice
  // versa). Probe on mount, every 20s, and whenever the tab regains focus.
  useEffect(() => {
    let alive = true;
    const probe = () =>
      getHealth()
        .then((h) => alive && setServerHealth(h.ok ? "online" : "offline"))
        .catch(() => alive && setServerHealth("offline"));
    probe();
    const id = setInterval(probe, 20_000);
    const onVis = () => {
      if (document.visibilityState === "visible") probe();
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      alive = false;
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, []);

  const setActiveProject = (p: Project | null) => {
    setActiveProjectState(p);
    try {
      if (p) localStorage.setItem(LS_KEY, JSON.stringify(p));
      else localStorage.removeItem(LS_KEY);
    } catch {
      /* ignore quota / private-mode errors */
    }
  };

  const value = useMemo<Store>(
    () => ({
      activeProject,
      setActiveProject,
      activeProjectRoot: activeProject ? projectPath(activeProject) || undefined : undefined,
      wsStatus,
      serverHealth,
      openSession,
      setOpenSession,
      newChatNonce,
      requestNewChat: () => setNewChatNonce((n) => n + 1),
    }),
    [activeProject, wsStatus, serverHealth, openSession, newChatNonce],
  );

  return <StoreCtx.Provider value={value}>{children}</StoreCtx.Provider>;
}

export function useStore(): Store {
  const ctx = useContext(StoreCtx);
  if (!ctx) throw new Error("useStore must be used within StoreProvider");
  return ctx;
}
