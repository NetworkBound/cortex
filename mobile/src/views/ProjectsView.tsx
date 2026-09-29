// Projects tab: list (branch, dirty count) → git status → per-file diff
// accordion; checkpoints (list / create / long-press restore); add a project
// from the server's discover list.

import { useEffect, useState } from "react";
import Icon from "../components/Icon";
import {
  ActionSheet,
  Banner,
  Chip,
  Empty,
  Field,
  GroupHead,
  HoldButton,
  Row,
  Scroll,
  Sheet,
  Skeleton,
  SubHeader,
} from "../components/ui";
import * as api from "../lib/api";
import { relTime } from "../lib/format";
import { errorMessage, isApiError } from "../lib/http";
import { haptic, openExternal } from "../lib/native";
import { back, enc, navigate, useRoute } from "../lib/nav";
import { useStore } from "../lib/store";
import { baseName, type GitFile, type Project } from "../lib/types";
import { useAsync } from "../lib/useAsync";

export default function ProjectsView() {
  const route = useRoute();
  const sub = route.rest[0];
  const root = route.params.get("root") ?? "";
  if (sub === "git" && root) return <GitStatusView root={root} />;
  if (sub === "diff" && root) {
    return <DiffView root={root} path={route.params.get("path") ?? ""} />;
  }
  if (sub === "checkpoints" && root) return <CheckpointsView root={root} />;
  if (sub === "discover") return <DiscoverView />;
  return <ProjectList />;
}

function ProjectList() {
  const {
    projects,
    refreshProjects,
    activeProjectRoot,
    setActiveProjectRoot,
    mode,
    resyncNonce,
  } = useStore();
  const [loading, setLoading] = useState(projects.length === 0);
  const [menu, setMenu] = useState<Project | null>(null);
  const legacy = mode === "legacy";
  const canGit = !legacy && api.hasFeature("git");
  const canCp = !legacy && api.hasFeature("checkpoints");
  const canAdd = !legacy && api.hasFeature("projects.add");

  useEffect(() => {
    refreshProjects().finally(() => setLoading(false));
  }, [refreshProjects, resyncNonce]);

  const openMenu = (p: Project) => {
    if (!canGit && !canCp) {
      setActiveProjectRoot(p.root === activeProjectRoot ? null : p.root);
      return;
    }
    setMenu(p);
  };

  return (
    <>
      <Scroll onRefresh={refreshProjects}>
        {loading ? (
          <Skeleton rows={4} />
        ) : projects.length === 0 ? (
          <Empty
            icon="folder"
            title="No projects"
            hint={
              canAdd
                ? "Add one from the folders Cortex found on the desktop."
                : "Add projects on the desktop app; they'll show up here."
            }
            action={
              canAdd
                ? {
                    label: "Add project",
                    onClick: () => navigate("/projects/discover"),
                  }
                : undefined
            }
          />
        ) : (
          <div className="list">
            {projects.map((p) => (
              <Row
                key={p.root}
                onClick={() => openMenu(p)}
                onLongPress={() => setActiveProjectRoot(p.root)}
                selected={p.root === activeProjectRoot}
                chevron={canGit || canCp}
                leading={
                  <Icon
                    name="folder"
                    size={20}
                    className={
                      p.root === activeProjectRoot ? "accent" : "muted"
                    }
                  />
                }
                title={
                  <span className="thread-title">
                    {p.name}
                    {p.trusted === false && <Chip tone="warn">untrusted</Chip>}
                  </span>
                }
                sub={
                  <span className="thread-sub">
                    {p.branch && (
                      <span className="proj">
                        <Icon name="git" size={12} /> {p.branch}
                      </span>
                    )}
                    {typeof p.dirty_files === "number" && p.dirty_files > 0 && (
                      <Chip tone="warn">{p.dirty_files} changed</Chip>
                    )}
                    {!p.branch && (p.subtitle || p.root)}
                  </span>
                }
                right={
                  p.root === activeProjectRoot ? (
                    <Icon name="check" size={18} className="accent" />
                  ) : undefined
                }
              />
            ))}
          </div>
        )}
        {canAdd && projects.length > 0 && (
          <div className="pad">
            <button
              className="btn block"
              onClick={() => navigate("/projects/discover")}
            >
              <Icon name="plus" size={18} /> Add project
            </button>
          </div>
        )}
      </Scroll>
      <ActionSheet
        open={!!menu}
        onClose={() => setMenu(null)}
        title={menu?.name}
        actions={[
          {
            label:
              menu?.root === activeProjectRoot
                ? "Active project ✓"
                : "Use for new chats",
            icon: "check",
            onClick: () => menu && setActiveProjectRoot(menu.root),
          },
          ...(canGit
            ? [
                {
                  label: "Git status",
                  icon: "git",
                  onClick: () =>
                    menu && navigate(`/projects/git?root=${enc(menu.root)}`),
                },
              ]
            : []),
          ...(canCp
            ? [
                {
                  label: "Checkpoints",
                  icon: "clock",
                  onClick: () =>
                    menu &&
                    navigate(`/projects/checkpoints?root=${enc(menu.root)}`),
                },
              ]
            : []),
        ]}
      />
    </>
  );
}

const STATUS_TONE: Record<string, "ok" | "warn" | "err" | "info" | "muted"> = {
  M: "warn",
  A: "ok",
  D: "err",
  R: "info",
  "??": "muted",
  U: "err",
};

function GitStatusView({ root }: { root: string }) {
  const { resyncNonce } = useStore();
  const { data, error, loading, refresh } = useAsync(
    () => api.gitStatus(root),
    [root, resyncNonce],
  );
  const files = data?.files ?? [];
  return (
    <>
      <SubHeader
        title={baseName(root)}
        sub={
          data ? (
            <>
              <Icon name="git" size={12} /> {data.branch ?? "detached"}
              {data.ahead ? ` · ↑${data.ahead}` : ""}
              {data.behind ? ` · ↓${data.behind}` : ""}
            </>
          ) : (
            "git status"
          )
        }
        onBack={() => back("/projects")}
        right={
          <button
            className="iconbtn"
            aria-label="Checkpoints"
            onClick={() => navigate(`/projects/checkpoints?root=${enc(root)}`)}
          >
            <Icon name="clock" />
          </button>
        }
      />
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={5} lines={1} />
        ) : files.length === 0 ? (
          <Empty
            icon="check"
            title="Working tree clean"
            hint="No uncommitted changes."
          />
        ) : (
          <div className="list">
            {files.map((f) => (
              <Row
                key={f.path}
                chevron
                onClick={() =>
                  navigate(
                    `/projects/diff?root=${enc(root)}&path=${enc(f.path)}`,
                  )
                }
                leading={
                  <Chip
                    tone={STATUS_TONE[f.status.trim()] ?? "muted"}
                    className="mono"
                  >
                    {f.status.trim() || "M"}
                  </Chip>
                }
                title={<span className="mono small">{f.path}</span>}
              />
            ))}
          </div>
        )}
      </Scroll>
    </>
  );
}

/** Unified diff, per-file accordion (all changed files, the tapped one open),
 *  sticky line-number gutter, ± colouring, wraps, 200 KB cap → open on desktop. */
function DiffView({ root, path }: { root: string; path: string }) {
  const { data: status } = useAsync(() => api.gitStatus(root), [root]);
  const files: GitFile[] = status?.files?.length
    ? status.files
    : [{ path, status: "M" }];
  const [open, setOpen] = useState<Set<string>>(new Set([path]));
  const toggle = (p: string) =>
    setOpen((s) => {
      const n = new Set(s);
      if (n.has(p)) n.delete(p);
      else n.add(p);
      return n;
    });
  return (
    <>
      <SubHeader
        title={baseName(path)}
        sub={baseName(root)}
        onBack={() => back(`/projects/git?root=${enc(root)}`)}
      />
      <Scroll>
        <div className="diff-list">
          {files.map((f) => (
            <details
              key={f.path}
              className="diff-file"
              open={open.has(f.path)}
              onToggle={(e) => {
                const isOpen = (e.currentTarget as HTMLDetailsElement).open;
                setOpen((s) => {
                  if (isOpen === s.has(f.path)) return s;
                  const n = new Set(s);
                  if (isOpen) n.add(f.path);
                  else n.delete(f.path);
                  return n;
                });
              }}
            >
              <summary
                onClick={(e) => {
                  e.preventDefault();
                  toggle(f.path);
                }}
              >
                <Chip
                  tone={STATUS_TONE[f.status.trim()] ?? "muted"}
                  className="mono"
                >
                  {f.status.trim() || "M"}
                </Chip>
                <span className="mono">{f.path}</span>
                <Icon name="chevron" size={16} className="diff-caret" />
              </summary>
              {open.has(f.path) && <FileDiff root={root} path={f.path} />}
            </details>
          ))}
        </div>
      </Scroll>
    </>
  );
}

function FileDiff({ root, path }: { root: string; path: string }) {
  const { data, error, loading } = useAsync(
    () => api.gitDiff(root, path),
    [root, path],
  );
  if (loading) return <Skeleton rows={3} lines={1} />;
  if (error) return <Banner kind="error">{error}</Banner>;
  const lines = (data?.diff ?? "").split("\n");
  let oldN = 0;
  let newN = 0;
  return (
    <div className="diff">
      {data?.truncated && (
        <Banner
          kind="warn"
          action={{
            label: "Open on desktop",
            onClick: () =>
              openExternal(
                `cortex://projects/diff?root=${enc(root)}&path=${enc(path)}`,
              ),
          }}
        >
          Diff over 200 KB — showing the first part.
        </Banner>
      )}
      {lines.length <= 1 && !data?.diff && (
        <div className="muted pad">No diff (binary or untracked file).</div>
      )}
      {lines.map((l, i) => {
        let cls = "ctx";
        let o = "";
        let n = "";
        const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(l);
        if (hunk) {
          oldN = parseInt(hunk[1], 10);
          newN = parseInt(hunk[2], 10);
          cls = "hunk";
        } else if (
          l.startsWith("+++") ||
          l.startsWith("---") ||
          l.startsWith("diff ") ||
          l.startsWith("index ")
        ) {
          cls = "meta";
        } else if (l.startsWith("+")) {
          cls = "add";
          n = String(newN++);
        } else if (l.startsWith("-")) {
          cls = "del";
          o = String(oldN++);
        } else if (l.startsWith("\\")) {
          cls = "meta";
        } else {
          o = String(oldN++);
          n = String(newN++);
        }
        return (
          <div className={`dl ${cls}`} key={i}>
            <span className="gut">{o}</span>
            <span className="gut">{n}</span>
            <span className="txt">{l || " "}</span>
          </div>
        );
      })}
    </div>
  );
}

function CheckpointsView({ root }: { root: string }) {
  const { toast, resyncNonce } = useStore();
  const { data, error, loading, refresh } = useAsync(
    () => api.listCheckpoints(root),
    [root, resyncNonce],
  );
  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [restoring, setRestoring] = useState<string | null>(null);

  const create = async () => {
    setBusy(true);
    try {
      await api.createCheckpoint(
        root,
        label.trim() || `Checkpoint ${new Date().toLocaleString()}`,
      );
      setCreating(false);
      setLabel("");
      haptic("success");
      refresh();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const restore = async (id: string) => {
    setRestoring(null);
    try {
      await api.restoreCheckpoint(id);
      toast("Checkpoint restored.", "success");
      refresh();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const list = data ?? [];
  return (
    <>
      <SubHeader
        title="Checkpoints"
        sub={baseName(root)}
        onBack={() => back(`/projects/git?root=${enc(root)}`)}
        right={
          <button
            className="iconbtn"
            aria-label="New checkpoint"
            onClick={() => setCreating(true)}
          >
            <Icon name="plus" />
          </button>
        }
      />
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={3} />
        ) : list.length === 0 ? (
          <Empty
            icon="clock"
            title="No checkpoints"
            hint="Snapshot the working tree before risky edits."
            action={{
              label: "Create checkpoint",
              onClick: () => setCreating(true),
            }}
          />
        ) : (
          <div className="list">
            {list.map((c) => (
              <Row
                key={c.id}
                title={c.label || c.id}
                sub={`${relTime(c.created_ms ?? c.ts_ms)}${typeof c.files === "number" ? ` · ${c.files} files` : ""}`}
                right={
                  <button
                    className="linkbtn"
                    onClick={() => setRestoring(c.id)}
                  >
                    Restore
                  </button>
                }
              />
            ))}
          </div>
        )}
      </Scroll>
      <Sheet
        open={creating}
        onClose={() => setCreating(false)}
        title="New checkpoint"
      >
        <Field label="Label">
          <input
            value={label}
            autoFocus
            placeholder="before refactor"
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && create()}
          />
        </Field>
        <button className="btn primary block" disabled={busy} onClick={create}>
          Create
        </button>
      </Sheet>
      <Sheet
        open={!!restoring}
        onClose={() => setRestoring(null)}
        title="Restore checkpoint?"
      >
        <p className="muted">
          This overwrites the working tree of <b>{baseName(root)}</b> with the
          checkpoint's files. Uncommitted changes since then are lost.
        </p>
        <HoldButton
          label="Hold to restore"
          holding="Keep holding…"
          className="danger block"
          onConfirm={() => restoring && restore(restoring)}
        />
      </Sheet>
    </>
  );
}

function DiscoverView() {
  const { refreshProjects, toast } = useStore();
  const { data, error, loading, refresh } = useAsync(
    () => api.discoverProjects(),
    [],
  );
  const [manual, setManual] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const add = async (root: string) => {
    setBusy(root);
    try {
      await api.addProject(root);
      await refreshProjects();
      haptic("success");
      toast(`Added ${baseName(root)}`, "success");
      back("/projects");
    } catch (e) {
      toast(
        isApiError(e, "not_found")
          ? "That folder doesn't exist on the desktop."
          : errorMessage(e),
        "error",
      );
    } finally {
      setBusy(null);
    }
  };

  const list = data ?? [];
  return (
    <>
      <SubHeader
        title="Add project"
        sub="Folders found on the desktop"
        onBack={() => back("/projects")}
      />
      <Scroll onRefresh={refresh}>
        {error && <Banner kind="error">{error}</Banner>}
        {loading ? (
          <Skeleton rows={4} lines={1} />
        ) : (
          <>
            {list.length > 0 && <GroupHead>Discovered</GroupHead>}
            <div className="list">
              {list.map((p) => (
                <Row
                  key={p.root}
                  title={p.name}
                  sub={<span className="mono small">{p.root}</span>}
                  right={
                    <button
                      className="linkbtn"
                      disabled={busy === p.root}
                      onClick={() => add(p.root)}
                    >
                      {busy === p.root ? "…" : "Add"}
                    </button>
                  }
                />
              ))}
            </div>
            <GroupHead>Or type a path</GroupHead>
            <div className="pad">
              <Field label="Folder on the desktop">
                <input
                  value={manual}
                  placeholder="C:\\code\\app or /home/me/app"
                  autoCapitalize="off"
                  autoCorrect="off"
                  onChange={(e) => setManual(e.target.value)}
                />
              </Field>
              <button
                className="btn primary block"
                disabled={!manual.trim() || !!busy}
                onClick={() => add(manual.trim())}
              >
                Add
              </button>
            </div>
          </>
        )}
      </Scroll>
    </>
  );
}
