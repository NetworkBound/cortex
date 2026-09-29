import { useEffect, useState } from "react";
import { TabBar, TopBar } from "./components/Shell";
import Icon from "./components/Icon";
import { Banner, Sheet, Toasts } from "./components/ui";
import { navigate, useRoute } from "./lib/nav";
import {
  enablePush,
  isIosSafariBrowser,
  pushState,
  pushSupported,
} from "./lib/push";
import { useStore } from "./lib/store";
import { hasFeature } from "./lib/api";
import { prefGet, prefSet } from "./lib/native";
import ChatsView from "./views/ChatsView";
import InboxView from "./views/InboxView";
import MoreView from "./views/MoreView";
import PairView from "./views/PairView";
import ProjectsView from "./views/ProjectsView";
import RunsView from "./views/RunsView";
import ThreadView from "./views/ThreadView";

/** Keep the layout keyboard-aware on iOS: size the app to the visual
 *  viewport so the composer sits above the keyboard instead of under it. */
function useVisualViewport() {
  useEffect(() => {
    const vv = window.visualViewport;
    const root = document.documentElement;
    const apply = () => {
      const h = vv ? vv.height : window.innerHeight;
      root.style.setProperty("--vvh", `${Math.round(h)}px`);
      // Prevent the page itself scrolling under the keyboard.
      if (vv && vv.offsetTop) window.scrollTo(0, 0);
    };
    apply();
    vv?.addEventListener("resize", apply);
    vv?.addEventListener("scroll", apply);
    window.addEventListener("resize", apply);
    return () => {
      vv?.removeEventListener("resize", apply);
      vv?.removeEventListener("scroll", apply);
      window.removeEventListener("resize", apply);
    };
  }, []);
}

/** Navigation requests from the service worker (notification taps). */
function useSwNavigation() {
  useEffect(() => {
    if (!("serviceWorker" in navigator)) return;
    const on = (e: MessageEvent) => {
      const d = e.data as { type?: string; path?: string } | null;
      if (d?.type === "navigate" && typeof d.path === "string")
        navigate(d.path);
    };
    navigator.serviceWorker.addEventListener("message", on);
    return () => navigator.serviceWorker.removeEventListener("message", on);
  }, []);
}

export default function App() {
  const { boot, toast } = useStore();
  const route = useRoute();
  useVisualViewport();
  useSwNavigation();

  // Post-pairing: offer push once (must come from a tap).
  const [askPush, setAskPush] = useState(false);
  useEffect(() => {
    if (boot !== "ready") return;
    let just = false;
    try {
      just = sessionStorage.getItem("cortex.justPaired") === "1";
      sessionStorage.removeItem("cortex.justPaired");
    } catch {
      /* ignore */
    }
    if (!just || !pushSupported() || !hasFeature("push.web")) return;
    pushState().then((s) => s === "default" && setAskPush(true));
  }, [boot]);

  // iOS Safari (not installed) hint, once.
  const [installHint, setInstallHint] = useState(
    () => isIosSafariBrowser() && !prefGet("cortex.installHintSeen", false),
  );

  // Already paired but opened via a stale `#/pair` link: land on chats.
  useEffect(() => {
    if (boot === "ready" && route.path === "/pair") {
      navigate("/chats", { replace: true });
    }
  }, [boot, route.path]);

  if (boot === "loading") {
    return (
      <div className="app splash" aria-busy="true">
        <span className="brand-mark">C</span>
        <span className="spin" />
      </div>
    );
  }
  if (boot === "pair" || boot === "unreachable") {
    return (
      <div className="app">
        <PairView />
        <Toasts />
      </div>
    );
  }

  // Routes with their own header (pushed screens) hide the tab chrome.
  const pushed =
    (route.tab === "chats" && route.rest.length > 0) ||
    (route.tab === "projects" && route.rest.length > 0) ||
    (route.tab === "runs" && route.rest.length > 0) ||
    (route.tab === "more" && route.rest.length > 0);

  let view: React.ReactNode;
  if (route.path === "/pair") {
    // Already paired; a stale pair link just lands on chats (see effect).
    view = null;
  } else if (route.tab === "chats" && route.rest[0]) {
    view = <ThreadView key={route.rest[0]} threadId={route.rest[0]} />;
  } else {
    switch (route.tab) {
      case "inbox":
        view = <InboxView />;
        break;
      case "projects":
        view = <ProjectsView />;
        break;
      case "runs":
        view = <RunsView />;
        break;
      case "more":
        view = <MoreView />;
        break;
      default:
        view = <ChatsView />;
    }
  }

  return (
    <div className="app">
      {!pushed && <TopBar />}
      {!pushed && installHint && (
        <Banner
          kind="info"
          action={{
            label: "How",
            onClick: () => {
              prefSet("cortex.installHintSeen", true);
              setInstallHint(false);
              navigate("/more/install");
            },
          }}
        >
          <button
            className="banner-close"
            aria-label="Dismiss"
            onClick={() => {
              prefSet("cortex.installHintSeen", true);
              setInstallHint(false);
            }}
          >
            <Icon name="x" size={14} />
          </button>
          Add Cortex to your Home Screen for notifications.
        </Banner>
      )}
      <main className="view">{view}</main>
      {!pushed && <TabBar active={route.tab} />}
      <Toasts />
      <Sheet
        open={askPush}
        onClose={() => setAskPush(false)}
        title="Turn on notifications?"
      >
        <p className="muted">
          Get a push when a run needs your approval, finishes, or fails — even
          with the app closed.
        </p>
        <button
          className="btn primary block"
          onClick={async () => {
            setAskPush(false);
            try {
              const s = await enablePush();
              if (s === "subscribed") toast("Notifications on.", "success");
            } catch (e) {
              toast(e instanceof Error ? e.message : String(e), "error");
            }
          }}
        >
          Turn on
        </button>
        <button className="linkbtn block" onClick={() => setAskPush(false)}>
          Not now
        </button>
      </Sheet>
    </div>
  );
}
