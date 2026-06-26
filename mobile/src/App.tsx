import { useState } from "react";
import { useStore } from "./lib/store";
import { useApprovalCount } from "./lib/useApprovalCount";
import { projectName } from "./lib/types";
import WsPill from "./components/WsPill";
import ChatView from "./views/ChatView";
import UltimateView from "./views/UltimateView";
import RecentView from "./views/RecentView";
import ProjectsView from "./views/ProjectsView";
import InboxView from "./views/InboxView";
import ImportView from "./views/ImportView";

type Tab = "chat" | "ultimate" | "recent" | "projects" | "inbox";
// "import" is a sub-screen of Recent (no bottom-nav slot), so it lives outside
// the `Tab` union and is tracked separately.

const TABS: { id: Tab; label: string; ico: string }[] = [
  { id: "chat", label: "Chat", ico: "💬" },
  { id: "ultimate", label: "Ultimate", ico: "✦" },
  { id: "recent", label: "Recent", ico: "🕑" },
  { id: "projects", label: "Projects", ico: "📁" },
  { id: "inbox", label: "Inbox", ico: "📥" },
];

export default function App() {
  const [tab, setTab] = useState<Tab>("chat");
  // When true, the Import sub-screen overlays the Recent slot. Bumping
  // `recentRefresh` forces RecentView to reload after an import.
  const [showImport, setShowImport] = useState(false);
  const [recentRefresh, setRecentRefresh] = useState(0);
  const { activeProject } = useStore();
  const approvals = useApprovalCount();

  const goTab = (t: Tab) => {
    setTab(t);
    if (t !== "recent") setShowImport(false);
  };

  return (
    <div className="app">
      <header className="app-header">
        {tab === "recent" && showImport ? (
          <button
            className="header-action"
            onClick={() => setShowImport(false)}
            aria-label="Back to Recent"
          >
            ‹ Back
          </button>
        ) : (
          <span className="brand">
            <span className="dot" />
            Cortex
          </span>
        )}
        <span className="spacer" />
        {activeProject && (
          <span className="ctx" title={projectName(activeProject)}>
            {projectName(activeProject)}
          </span>
        )}
        <WsPill />
      </header>

      {/* All views stay mounted so chat streams / ultimate runs survive tab
          switches; only the active one is shown. */}
      <div className="view" style={view(tab === "chat")}>
        <ChatView />
      </div>
      <div className="view" style={view(tab === "ultimate")}>
        <UltimateView />
      </div>
      <div className="view" style={view(tab === "recent" && !showImport)}>
        <RecentView
          refreshKey={recentRefresh}
          onOpen={() => setTab("chat")}
          onImport={() => setShowImport(true)}
        />
      </div>
      <div className="view" style={view(tab === "recent" && showImport)}>
        <ImportView
          onImported={() => {
            // Refresh Recent, then drop back to it so imported chats show.
            setRecentRefresh((n) => n + 1);
          }}
        />
      </div>
      <div className="view" style={view(tab === "projects")}>
        <ProjectsView />
      </div>
      <div className="view" style={view(tab === "inbox")}>
        <InboxView />
      </div>

      <nav className="tabbar">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={tab === t.id ? "active" : ""}
            onClick={() => goTab(t.id)}
          >
            <span className="ico">{t.ico}</span>
            {t.label}
            {t.id === "inbox" && approvals > 0 && (
              <span className="badge">{approvals}</span>
            )}
          </button>
        ))}
      </nav>
    </div>
  );
}

function view(active: boolean): React.CSSProperties {
  return active ? {} : { display: "none" };
}
