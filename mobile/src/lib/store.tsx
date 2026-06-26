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
import type { Project } from "./types";
import { projectPath } from "./types";

interface Store {
  activeProject: Project | null;
  setActiveProject: (p: Project | null) => void;
  activeProjectRoot: string | undefined;
  wsStatus: WsStatus;
  /** A session the Recent tab asked the Chat view to open + resume, or null.
   *  The Chat view loads its history then clears it via `setOpenSession(null)`. */
  openSession: string | null;
  setOpenSession: (id: string | null) => void;
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
  const [openSession, setOpenSession] = useState<string | null>(null);

  useEffect(() => {
    bus.connect();
    return bus.onStatus(setWsStatus);
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
      openSession,
      setOpenSession,
    }),
    [activeProject, wsStatus, openSession],
  );

  return <StoreCtx.Provider value={value}>{children}</StoreCtx.Provider>;
}

export function useStore(): Store {
  const ctx = useContext(StoreCtx);
  if (!ctx) throw new Error("useStore must be used within StoreProvider");
  return ctx;
}
