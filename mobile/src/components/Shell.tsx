// App chrome: top bar (status pill + project switcher) and the bottom tab bar.

import { useState } from "react";
import Icon from "./Icon";
import { ActionSheet } from "./ui";
import { navigate, type Tab } from "../lib/nav";
import { useStore } from "../lib/store";
import { baseName } from "../lib/types";
import { haptic } from "../lib/native";

export function StatusPill() {
  const { connection, demo } = useStore();
  const label =
    connection === "connected"
      ? demo
        ? "Demo"
        : "Live"
      : connection === "reconnecting"
        ? "Reconnecting"
        : "Offline";
  return (
    <span
      className={`status-pill ${connection}`}
      role="status"
      aria-label={`Server ${label}`}
    >
      <span className="led" />
      {label}
    </span>
  );
}

export function ProjectSwitcher() {
  const { projects, activeProjectRoot, setActiveProjectRoot } = useStore();
  const [open, setOpen] = useState(false);
  const name = activeProjectRoot ? baseName(activeProjectRoot) : "All projects";
  return (
    <>
      <button
        className="proj-switch"
        onClick={() => setOpen(true)}
        aria-label={`Project: ${name}. Change project`}
      >
        <Icon name="folder" size={16} />
        <span className="proj-name">{name}</span>
        <Icon name="arrowDown" size={14} className="proj-caret" />
      </button>
      <ActionSheet
        open={open}
        onClose={() => setOpen(false)}
        title="Project"
        actions={[
          {
            label: "All projects",
            icon: activeProjectRoot ? undefined : "check",
            onClick: () => setActiveProjectRoot(null),
          },
          ...projects.map((p) => ({
            label: p.name,
            icon: p.root === activeProjectRoot ? "check" : undefined,
            onClick: () => setActiveProjectRoot(p.root),
          })),
        ]}
      />
    </>
  );
}

export function TopBar({
  title,
  right,
}: {
  title?: string;
  right?: React.ReactNode;
}) {
  return (
    <header className="topbar">
      {title ? (
        <span className="brand">{title}</span>
      ) : (
        <span className="brand">
          <span className="dot" />
          Cortex
        </span>
      )}
      <span className="spacer" />
      <ProjectSwitcher />
      {right}
      <StatusPill />
    </header>
  );
}

const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: "chats", label: "Chats", icon: "chat" },
  { id: "inbox", label: "Inbox", icon: "inbox" },
  { id: "projects", label: "Projects", icon: "folder" },
  { id: "runs", label: "Runs", icon: "activity" },
  { id: "more", label: "More", icon: "more" },
];

export function TabBar({ active }: { active: Tab }) {
  const { inboxCount } = useStore();
  return (
    <nav className="tabbar" aria-label="Main">
      {TABS.map((t) => {
        const badge = t.id === "inbox" ? inboxCount : 0;
        return (
          <button
            key={t.id}
            className={active === t.id ? "active" : ""}
            onClick={() => {
              haptic("selection");
              navigate(`/${t.id}`, { replace: active === t.id });
            }}
            aria-current={active === t.id ? "page" : undefined}
            aria-label={
              badge > 0 ? `${t.label}, ${badge} needing attention` : t.label
            }
          >
            <span className="tab-ico">
              <Icon name={t.icon} />
              {badge > 0 && (
                <span className="badge" aria-hidden="true">
                  {badge > 99 ? "99+" : badge}
                </span>
              )}
            </span>
            {t.label}
          </button>
        );
      })}
    </nav>
  );
}
