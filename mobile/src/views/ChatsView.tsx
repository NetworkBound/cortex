// Thread list: title, project, agent chip, relative time, running dot,
// approval badge. Long-press / swipe for rename + delete. "+" starts a chat
// with a project + model picker.

import { useCallback, useEffect, useMemo, useState } from "react";
import Icon from "../components/Icon";
import {
  ActionSheet,
  Banner,
  Chip,
  Empty,
  Field,
  Row,
  Scroll,
  Sheet,
  Skeleton,
} from "../components/ui";
import * as api from "../lib/api";
import { relTime } from "../lib/format";
import { errorMessage } from "../lib/http";
import { haptic } from "../lib/native";
import { enc, navigate } from "../lib/nav";
import { useStore } from "../lib/store";
import { useAsync } from "../lib/useAsync";
import { useWs } from "../lib/useWs";
import { baseName, type Model, type Thread } from "../lib/types";

export default function ChatsView() {
  const { activeProjectRoot, resyncNonce, mode, projects, toast } = useStore();
  const legacy = mode === "legacy";
  const { data, setData, error, loading, refresh } = useAsync(
    () => api.listThreads(activeProjectRoot ?? undefined),
    [activeProjectRoot, resyncNonce],
  );
  const [menu, setMenu] = useState<Thread | null>(null);
  const [rename, setRename] = useState<Thread | null>(null);
  const [title, setTitle] = useState("");
  const [showNew, setShowNew] = useState(false);

  useWs((ev) => {
    if (ev.type === "thread_updated") {
      setData((d) => {
        if (!d) return d;
        const i = d.threads.findIndex((t) => t.id === ev.thread.id);
        const threads = [...d.threads];
        if (i >= 0) threads[i] = { ...threads[i], ...ev.thread };
        else threads.unshift(ev.thread);
        threads.sort((a, b) => (b.last_ms ?? 0) - (a.last_ms ?? 0));
        return { ...d, threads };
      });
    } else if (ev.type === "done" || ev.type === "approval_request") {
      refresh();
    }
  });

  const threads = data?.threads ?? [];

  const doRename = async () => {
    if (!rename) return;
    const t = title.trim();
    setRename(null);
    if (!t || t === rename.title) return;
    try {
      await api.renameThread(rename.id, t);
      setData((d) =>
        d
          ? {
              ...d,
              threads: d.threads.map((x) =>
                x.id === rename.id ? { ...x, title: t } : x,
              ),
            }
          : d,
      );
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const doDelete = async (t: Thread) => {
    try {
      await api.deleteThread(t.id);
      setData((d) =>
        d ? { ...d, threads: d.threads.filter((x) => x.id !== t.id) } : d,
      );
      haptic("success");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const open = (t: Thread) => navigate(`/threads/${enc(t.id)}`);

  return (
    <>
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={6} />
        ) : threads.length === 0 ? (
          <Empty
            icon="chat"
            title="No chats yet"
            hint={
              activeProjectRoot
                ? `Nothing in ${baseName(activeProjectRoot)} yet.`
                : "Start a conversation with your Cortex."
            }
            action={{ label: "New chat", onClick: () => setShowNew(true) }}
          />
        ) : (
          <div className="list">
            {threads.map((t) => (
              <Row
                key={t.id}
                onClick={() => open(t)}
                onLongPress={legacy ? undefined : () => setMenu(t)}
                onSwipeLeft={legacy ? undefined : () => setMenu(t)}
                title={
                  <span className="thread-title">
                    {t.running && (
                      <span className="run-dot" aria-label="Running" />
                    )}
                    {t.title || "New chat"}
                  </span>
                }
                sub={
                  <span className="thread-sub">
                    {t.project_root && (
                      <span className="proj">{baseName(t.project_root)}</span>
                    )}
                    {t.agent_id && <Chip tone="muted">{t.agent_id}</Chip>}
                    {t.last_preview && (
                      <span className="prev">{t.last_preview}</span>
                    )}
                  </span>
                }
                right={
                  <span className="thread-right">
                    <span className="time">{relTime(t.last_ms)}</span>
                    {(t.pending_approvals ?? 0) > 0 && (
                      <span className="badge">{t.pending_approvals}</span>
                    )}
                  </span>
                }
              />
            ))}
          </div>
        )}
      </Scroll>

      <button
        className="fab"
        aria-label="New chat"
        onClick={() => {
          haptic("light");
          setShowNew(true);
        }}
      >
        <Icon name="plus" size={24} />
      </button>

      <ActionSheet
        open={!!menu}
        onClose={() => setMenu(null)}
        title={menu?.title}
        actions={[
          {
            label: "Rename",
            icon: "edit",
            onClick: () => {
              if (menu) {
                setTitle(menu.title);
                setRename(menu);
              }
            },
          },
          {
            label: "Delete",
            icon: "trash",
            destructive: true,
            onClick: () => menu && doDelete(menu),
          },
        ]}
      />

      <Sheet
        open={!!rename}
        onClose={() => setRename(null)}
        title="Rename chat"
      >
        <Field label="Title">
          <input
            value={title}
            autoFocus
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && doRename()}
          />
        </Field>
        <button className="btn primary block" onClick={doRename}>
          Save
        </button>
      </Sheet>

      <NewChatSheet
        open={showNew}
        onClose={() => setShowNew(false)}
        projects={projects.map((p) => ({ root: p.root, name: p.name }))}
        defaultRoot={activeProjectRoot}
      />
    </>
  );
}

export function NewChatSheet({
  open,
  onClose,
  projects,
  defaultRoot,
}: {
  open: boolean;
  onClose: () => void;
  projects: { root: string; name: string }[];
  defaultRoot: string | null;
}) {
  const { mode, toast } = useStore();
  const [root, setRoot] = useState<string>(defaultRoot ?? "");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<Model[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => setRoot(defaultRoot ?? ""), [defaultRoot, open]);
  useEffect(() => {
    if (!open || models.length) return;
    api
      .listModels()
      .then((r) => {
        setModels(r.models);
        if (r.default) setModel((m) => m || r.default!);
      })
      .catch(() => {});
  }, [open, models.length]);

  const start = useCallback(async () => {
    setBusy(true);
    try {
      if (mode === "legacy") {
        // Legacy servers create the session on the first message.
        onClose();
        navigate(
          `/threads/new?${new URLSearchParams({ root, model }).toString()}`,
        );
        return;
      }
      const t = await api.createThread(root || undefined);
      onClose();
      navigate(`/threads/${enc(t.id)}${model ? `?model=${enc(model)}` : ""}`);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  }, [mode, root, model, onClose, toast]);

  const grouped = useMemo(() => models, [models]);

  return (
    <Sheet open={open} onClose={onClose} title="New chat">
      <Field label="Project">
        <select value={root} onChange={(e) => setRoot(e.target.value)}>
          <option value="">No project</option>
          {projects.map((p) => (
            <option key={p.root} value={p.root}>
              {p.name}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Model">
        <select value={model} onChange={(e) => setModel(e.target.value)}>
          <option value="">Default (router decides)</option>
          {grouped.map((m) => (
            <option key={m.id} value={m.id}>
              {m.label}
              {m.provider ? ` · ${m.provider}` : ""}
              {m.local ? " · local" : ""}
            </option>
          ))}
        </select>
      </Field>
      <button className="btn primary block" disabled={busy} onClick={start}>
        Start chat
      </button>
    </Sheet>
  );
}
