// Hash router. The URL hash is the single source of truth for navigation so
// the Android back button / browser history, PWA `start_url` ("/#/chats") and
// deep links (`#/pair?code=…`, `#/threads/<id>`, `#/inbox`) all just work.
//
//   #/chats                      thread list
//   #/threads/<id>[?approval=id] one thread (optionally focus an approval)
//   #/inbox                      needs-attention list
//   #/projects[/git|/diff|/checkpoints|/discover]?root=&path=
//   #/runs[/<run_id>]
//   #/more[/routines[/<id>|/new]|/devices|/about|/import|/install]
//   #/pair?url=&code=            pairing screen
//   #/approvals/<id>             → inbox, focused on that approval

import { useEffect, useState } from "react";

export type Tab = "chats" | "inbox" | "projects" | "runs" | "more";

export interface Route {
  tab: Tab;
  /** Path segments after the tab, e.g. ["git"] or ["<id>"]. */
  rest: string[];
  params: URLSearchParams;
  /** Full path incl. leading "/", without the query. */
  path: string;
}

const TABS: Tab[] = ["chats", "inbox", "projects", "runs", "more"];

export function parseHash(hash: string): Route {
  const raw = hash.replace(/^#/, "");
  const [pathPart, query = ""] = raw.split("?");
  const path = pathPart.startsWith("/") ? pathPart : `/${pathPart}`;
  const segs = path.split("/").filter(Boolean).map(decodeURIComponent);
  const params = new URLSearchParams(query);
  let head = segs[0] ?? "chats";
  let rest = segs.slice(1);
  // Aliases that map onto a tab.
  if (head === "threads") {
    head = "chats";
  } else if (head === "approvals") {
    head = "inbox";
  } else if (head === "chat") {
    head = "chats";
    rest = [];
  }
  const tab = (TABS as string[]).includes(head) ? (head as Tab) : "chats";
  return { tab, rest, params, path };
}

/** Kind of screen the route resolves to (used by App to pick a view). */
export function currentHash(): string {
  return location.hash || "#/chats";
}

let depth = 0;

/** Navigate to a hash path (pushes a history entry). */
export function navigate(path: string, opts: { replace?: boolean } = {}) {
  const target = `#${path.startsWith("/") ? path : `/${path}`}`;
  if (target === location.hash) return;
  if (opts.replace) {
    history.replaceState(null, "", target);
    // replaceState doesn't fire hashchange.
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  } else {
    depth += 1;
    location.hash = target;
  }
}

/** Go back to the previous screen, or to the tab root when there is none
 *  (cold start on a deep link). */
export function back(fallback?: string) {
  if (depth > 0) {
    depth -= 1;
    history.back();
    return;
  }
  const r = parseHash(currentHash());
  navigate(fallback ?? `/${r.tab}`, { replace: true });
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseHash(currentHash()));
  useEffect(() => {
    const on = () => setRoute(parseHash(currentHash()));
    window.addEventListener("hashchange", on);
    window.addEventListener("popstate", on);
    return () => {
      window.removeEventListener("hashchange", on);
      window.removeEventListener("popstate", on);
    };
  }, []);
  return route;
}

/** Map a `cortex://…` deep link (or an `#/…` hash) to an in-app hash path. */
export function deepLinkToPath(link: string): string | null {
  const s = link.trim();
  if (!s) return null;
  const m = /^cortex:\/\/([^?#]*)(\?.*)?$/i.exec(s);
  if (m) {
    const path = `/${m[1].replace(/^\/+|\/+$/g, "")}`;
    return `${path}${m[2] ?? ""}`;
  }
  if (s.startsWith("#")) return s.slice(1);
  if (s.startsWith("/")) return s;
  return null;
}

export const enc = encodeURIComponent;
