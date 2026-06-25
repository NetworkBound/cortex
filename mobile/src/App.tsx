import { useState } from "react";
import { useStore } from "./lib/store";
import { useApprovalCount } from "./lib/useApprovalCount";
import { projectName } from "./lib/types";
import WsPill from "./components/WsPill";
import ChatView from "./views/ChatView";
import UltimateView from "./views/UltimateView";
import ProjectsView from "./views/ProjectsView";
import InboxView from "./views/InboxView";

type Tab = "chat" | "ultimate" | "projects" | "inbox";

const TABS: { id: Tab; label: string; ico: string }[] = [
  { id: "chat", label: "Chat", ico: "💬" },
  { id: "ultimate", label: "Ultimate", ico: "✦" },
  { id: "projects", label: "Projects", ico: "📁" },
  { id: "inbox", label: "Inbox", ico: "📥" },
];

export default function App() {
  const [tab, setTab] = useState<Tab>("chat");
  const { activeProject } = useStore();
  const approvals = useApprovalCount();

  return (
    <div className="app">
      <header className="app-header">
        <span className="brand">
          <span className="dot" />
          Cortex
        </span>
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
            onClick={() => setTab(t.id)}
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
