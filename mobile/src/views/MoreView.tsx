// More: routines, defaults (model / plan mode), notifications, devices,
// theme, server info, install hint, chat import, sign out.

import { useEffect, useRef, useState } from "react";
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
  Segmented,
  Sheet,
  Skeleton,
  SubHeader,
  Toggle,
} from "../components/ui";
import * as api from "../lib/api";
import { relTime, untilTime } from "../lib/format";
import { errorMessage } from "../lib/http";
import { haptic, isNativeShell } from "../lib/native";
import { back, enc, navigate, useRoute } from "../lib/nav";
import {
  disablePush,
  enablePush,
  isIosSafariBrowser,
  pushState,
  type PushState,
} from "../lib/push";
import { useStore, type Theme } from "../lib/store";
import type { Model, Routine } from "../lib/types";
import { useAsync } from "../lib/useAsync";

export default function MoreView() {
  const route = useRoute();
  const [a, b] = route.rest;
  if (a === "routines" && b) return <RoutineEdit id={b === "new" ? null : b} />;
  if (a === "routines") return <RoutinesList />;
  if (a === "devices") return <DevicesView />;
  if (a === "about") return <AboutView />;
  if (a === "import") return <ImportView />;
  if (a === "install") return <InstallView />;
  return <MoreHome />;
}

function MoreHome() {
  const { mode, caps, server, theme, setTheme, signOut, toast, demo } =
    useStore();
  const legacy = mode === "legacy";
  const [settings, setSettings] = useState<{
    default_model?: string | null;
    plan_mode?: boolean;
  } | null>(null);
  const [models, setModels] = useState<Model[]>([]);
  const [modelSheet, setModelSheet] = useState(false);
  const [push, setPush] = useState<PushState>("unsupported");
  const [pushInfo, setPushInfo] = useState<{
    provider?: string | null;
    enabled?: boolean;
  } | null>(null);
  const [signout, setSignout] = useState(false);

  useEffect(() => {
    if (legacy) return;
    api
      .getSettings()
      .then(setSettings)
      .catch(() => setSettings(null));
    api
      .listModels()
      .then((r) => setModels(r.models))
      .catch(() => {});
    api
      .pushStatus()
      .then(setPushInfo)
      .catch(() => {});
  }, [legacy]);
  useEffect(() => {
    pushState().then(setPush);
  }, []);

  const save = async (patch: {
    default_model?: string | null;
    plan_mode?: boolean;
  }) => {
    const next = { ...(settings ?? {}), ...patch };
    setSettings(next);
    try {
      await api.putSettings(next);
      haptic("success");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const togglePush = async () => {
    try {
      if (push === "subscribed") {
        await disablePush();
        setPush("granted");
      } else {
        const s = await enablePush();
        setPush(s);
        if (s === "subscribed") toast("Notifications on.", "success");
        else if (s === "denied")
          toast("Notifications are blocked in your browser settings.", "error");
      }
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const modelLabel =
    models.find((m) => m.id === settings?.default_model)?.label ??
    settings?.default_model ??
    "Auto";
  const webPush = !legacy && api.hasFeature("push.web");

  return (
    <>
      <Scroll>
        <GroupHead>Automation</GroupHead>
        <div className="list">
          <Row
            chevron
            leading={<Icon name="clock" size={20} className="muted" />}
            title="Routines"
            sub={legacy ? "Needs a newer Cortex" : "Scheduled prompts"}
            onClick={() => !legacy && navigate("/more/routines")}
          />
        </div>

        {!legacy && (
          <>
            <GroupHead>Defaults</GroupHead>
            <div className="list">
              <Row
                leading={<Icon name="bolt" size={20} className="muted" />}
                title="Default model"
                sub={modelLabel}
                chevron
                onClick={() => setModelSheet(true)}
              />
              <Row
                leading={<Icon name="edit" size={20} className="muted" />}
                title="Plan mode"
                sub="Agents propose a plan before editing"
                right={
                  <Toggle
                    checked={!!settings?.plan_mode}
                    onChange={(v) => save({ plan_mode: v })}
                    label="Plan mode"
                  />
                }
              />
            </div>
          </>
        )}

        <GroupHead>Notifications</GroupHead>
        <div className="list">
          {push !== "unsupported" && (
            <Row
              leading={<Icon name="bell" size={20} className="muted" />}
              title="Push to this device"
              sub={
                !webPush
                  ? "Server doesn't support web push yet"
                  : push === "subscribed"
                    ? "On"
                    : push === "denied"
                      ? "Blocked in browser settings"
                      : "Approvals, finished and failed runs"
              }
              right={
                <Toggle
                  checked={push === "subscribed"}
                  onChange={togglePush}
                  disabled={!webPush || push === "denied"}
                  label="Push notifications"
                />
              }
            />
          )}
          <Row
            leading={<Icon name="phone" size={20} className="muted" />}
            title="Desktop push (ntfy / Gotify)"
            sub={
              pushInfo?.enabled
                ? `Configured via ${pushInfo.provider ?? "ntfy"} on the desktop — taps open the Inbox here.`
                : "Set up in desktop Settings → Notifications to get pushes via the ntfy app."
            }
          />
          {isIosSafariBrowser() && (
            <Row
              chevron
              leading={<Icon name="upload" size={20} className="muted" />}
              title="Install on iPhone"
              sub="Add to Home Screen for notifications + full screen"
              onClick={() => navigate("/more/install")}
            />
          )}
        </div>

        <GroupHead>Appearance</GroupHead>
        <div className="pad">
          <Segmented<Theme>
            value={theme}
            onChange={setTheme}
            options={[
              { value: "system", label: "System" },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" },
            ]}
          />
        </div>

        <GroupHead>Server</GroupHead>
        <div className="list">
          <Row
            chevron
            leading={<Icon name="phone" size={20} className="muted" />}
            title="Devices"
            sub={legacy ? "Needs a newer Cortex" : "Paired phones and tablets"}
            onClick={() => !legacy && navigate("/more/devices")}
          />
          <Row
            chevron
            leading={<Icon name="info" size={20} className="muted" />}
            title={server.server_name || (demo ? "Demo Cortex" : "Cortex")}
            sub={`v${caps?.server_version ?? server.server_version ?? "?"} · ${legacy ? "legacy API" : "API v2"}${server.url ? ` · ${server.url.replace(/^https?:\/\//, "")}` : ""}`}
            onClick={() => navigate("/more/about")}
          />
          <Row
            chevron
            leading={<Icon name="upload" size={20} className="muted" />}
            title="Import chat history"
            sub="Claude.ai / ChatGPT export"
            onClick={() => navigate("/more/import")}
          />
          <Row
            leading={<Icon name="logout" size={20} className="err" />}
            title={
              <span className="err">{demo ? "Leave demo" : "Sign out"}</span>
            }
            sub={demo ? undefined : "Revokes this device's token"}
            onClick={() => setSignout(true)}
          />
        </div>
        <div className="pad muted small center">
          {isNativeShell()
            ? "Cortex mobile (native shell)"
            : "Cortex mobile (PWA)"}
        </div>
      </Scroll>

      <ActionSheet
        open={modelSheet}
        onClose={() => setModelSheet(false)}
        title="Default model"
        actions={[
          {
            label: "Auto (router decides)",
            icon: settings?.default_model ? undefined : "check",
            onClick: () => save({ default_model: null }),
          },
          ...models.map((m) => ({
            label: `${m.label}${m.provider ? ` · ${m.provider}` : ""}`,
            icon: m.id === settings?.default_model ? "check" : undefined,
            onClick: () => save({ default_model: m.id }),
          })),
        ]}
      />
      <Sheet
        open={signout}
        onClose={() => setSignout(false)}
        title={demo ? "Leave the demo?" : "Sign out?"}
      >
        <p className="muted">
          {demo
            ? "You'll be back at the pairing screen."
            : "This phone's token is revoked on the desktop. Pair again from Settings → Mobile to reconnect."}
        </p>
        <HoldButton
          label={demo ? "Hold to leave" : "Hold to sign out"}
          holding="Keep holding…"
          className="danger block"
          onConfirm={() => {
            setSignout(false);
            signOut();
          }}
        />
      </Sheet>
    </>
  );
}

// ── Routines ───────────────────────────────────────────────────────────────

function RoutinesList() {
  const { toast, resyncNonce } = useStore();
  const { data, setData, error, loading, refresh } = useAsync(
    () => api.listRoutines(),
    [resyncNonce],
  );
  const [busy, setBusy] = useState<string | null>(null);

  const toggle = async (r: Routine, enabled: boolean) => {
    setData((d) => d?.map((x) => (x.id === r.id ? { ...x, enabled } : x)) ?? d);
    try {
      await api.updateRoutine(r.id, { ...r, enabled });
      refresh();
    } catch (e) {
      toast(errorMessage(e), "error");
      refresh();
    }
  };
  const runNow = async (r: Routine) => {
    setBusy(r.id);
    try {
      await api.runRoutine(r.id);
      haptic("success");
      toast(`Running "${r.name}"…`, "success");
      setTimeout(refresh, 1500);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const list = data ?? [];
  return (
    <>
      <SubHeader
        title="Routines"
        onBack={() => back("/more")}
        right={
          <button
            className="iconbtn"
            aria-label="New routine"
            onClick={() => navigate("/more/routines/new")}
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
            title="No routines"
            hint="Schedule a prompt to run daily or every few hours."
            action={{
              label: "New routine",
              onClick: () => navigate("/more/routines/new"),
            }}
          />
        ) : (
          <div className="list">
            {list.map((r) => (
              <Row
                key={r.id}
                onClick={() => navigate(`/more/routines/${enc(r.id)}`)}
                title={
                  <span className="thread-title">
                    {r.name || "Untitled"}
                    {r.last_status === "error" && (
                      <Chip tone="err">failed</Chip>
                    )}
                  </span>
                }
                sub={
                  <span className="thread-sub">
                    <span>{schedule(r)}</span>
                    {r.enabled && r.next_run_unix_ms ? (
                      <span>· next {untilTime(r.next_run_unix_ms)}</span>
                    ) : null}
                    {r.last_run_unix_ms ? (
                      <span>· last {relTime(r.last_run_unix_ms)}</span>
                    ) : null}
                  </span>
                }
                right={
                  <span
                    className="row-actions"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      className="iconbtn"
                      aria-label={`Run ${r.name} now`}
                      disabled={busy === r.id}
                      onClick={() => runNow(r)}
                    >
                      <Icon name="play" size={18} />
                    </button>
                    <Toggle
                      checked={r.enabled}
                      onChange={(v) => toggle(r, v)}
                      label={`Enable ${r.name}`}
                    />
                  </span>
                }
              />
            ))}
          </div>
        )}
      </Scroll>
    </>
  );
}

function schedule(r: Routine): string {
  if (r.daily_at) return `daily at ${r.daily_at}`;
  if (!r.interval_minutes) return "manual";
  if (r.interval_minutes % 1440 === 0)
    return `every ${r.interval_minutes / 1440}d`;
  if (r.interval_minutes % 60 === 0) return `every ${r.interval_minutes / 60}h`;
  return `every ${r.interval_minutes}m`;
}

function RoutineEdit({ id }: { id: string | null }) {
  const { toast, projects, caps } = useStore();
  const [r, setR] = useState<Partial<Routine>>({
    name: "",
    prompt: "",
    interval_minutes: 0,
    enabled: true,
    daily_at: "09:00",
  });
  const [mode, setMode] = useState<"daily" | "interval" | "manual">("daily");
  const [loading, setLoading] = useState(!!id);
  const [busy, setBusy] = useState(false);
  const [history, setHistory] = useState<
    {
      ts_ms?: number;
      started_ms?: number;
      status?: string;
      output?: string;
      error?: string;
    }[]
  >([]);
  const [del, setDel] = useState(false);
  const agents = caps?.local_agents ?? [];

  useEffect(() => {
    if (!id) return;
    api
      .listRoutines()
      .then((list) => {
        const hit = list.find((x) => x.id === id);
        if (hit) {
          setR(hit);
          setMode(
            hit.daily_at
              ? "daily"
              : hit.interval_minutes
                ? "interval"
                : "manual",
          );
        }
      })
      .catch((e) => toast(errorMessage(e), "error"))
      .finally(() => setLoading(false));
    api
      .routineHistory(id)
      .then(setHistory)
      .catch(() => {});
  }, [id, toast]);

  const save = async () => {
    if (!r.name?.trim() || !r.prompt?.trim()) {
      toast("Give the routine a name and a prompt.", "error");
      return;
    }
    const spec: Partial<Routine> = {
      ...r,
      daily_at: mode === "daily" ? r.daily_at || "09:00" : null,
      interval_minutes: mode === "interval" ? r.interval_minutes || 60 : 0,
      agent_id: r.agent_id || null,
      project_root: r.project_root || null,
    };
    setBusy(true);
    try {
      if (id) await api.updateRoutine(id, spec);
      else await api.createRoutine(spec);
      haptic("success");
      back("/more/routines");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!id) return;
    try {
      await api.deleteRoutine(id);
      back("/more/routines");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  return (
    <>
      <SubHeader
        title={id ? "Edit routine" : "New routine"}
        onBack={() => back("/more/routines")}
        right={
          id ? (
            <button
              className="iconbtn"
              aria-label="Delete routine"
              onClick={() => setDel(true)}
            >
              <Icon name="trash" />
            </button>
          ) : undefined
        }
      />
      <Scroll>
        {loading ? (
          <Skeleton rows={4} />
        ) : (
          <div className="pad form">
            <Field label="Name">
              <input
                value={r.name ?? ""}
                onChange={(e) => setR({ ...r, name: e.target.value })}
                placeholder="Morning triage"
              />
            </Field>
            <Field label="Prompt">
              <textarea
                rows={4}
                value={r.prompt ?? ""}
                onChange={(e) => setR({ ...r, prompt: e.target.value })}
                placeholder="Summarise open issues and failing CI runs."
              />
            </Field>
            <Field label="Schedule">
              <Segmented
                value={mode}
                onChange={setMode}
                options={[
                  { value: "daily", label: "Daily at" },
                  { value: "interval", label: "Every" },
                  { value: "manual", label: "Manual" },
                ]}
              />
            </Field>
            {mode === "daily" && (
              <Field label="Time (desktop local time)">
                <input
                  type="time"
                  value={r.daily_at ?? "09:00"}
                  onChange={(e) => setR({ ...r, daily_at: e.target.value })}
                />
              </Field>
            )}
            {mode === "interval" && (
              <Field label="Interval">
                <select
                  value={String(r.interval_minutes || 60)}
                  onChange={(e) =>
                    setR({ ...r, interval_minutes: Number(e.target.value) })
                  }
                >
                  {[15, 30, 60, 120, 240, 480, 720, 1440].map((m) => (
                    <option key={m} value={m}>
                      {m < 60 ? `${m} min` : m < 1440 ? `${m / 60} h` : "1 day"}
                    </option>
                  ))}
                </select>
              </Field>
            )}
            <Field label="Agent">
              <select
                value={r.agent_id ?? ""}
                onChange={(e) =>
                  setR({ ...r, agent_id: e.target.value || null })
                }
              >
                <option value="">Default (gateway or first local agent)</option>
                {agents.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Project">
              <select
                value={r.project_root ?? ""}
                onChange={(e) =>
                  setR({ ...r, project_root: e.target.value || null })
                }
              >
                <option value="">None</option>
                {projects.map((p) => (
                  <option key={p.root} value={p.root}>
                    {p.name}
                  </option>
                ))}
              </select>
            </Field>
            <div className="row-line">
              <span>Enabled</span>
              <Toggle
                checked={!!r.enabled}
                onChange={(v) => setR({ ...r, enabled: v })}
                label="Enabled"
              />
            </div>
            <button
              className="btn primary block"
              disabled={busy}
              onClick={save}
            >
              {id ? "Save" : "Create"}
            </button>
            {r.last_error && (
              <Banner kind="error">Last run: {r.last_error}</Banner>
            )}
            {r.last_output && (
              <details className="card">
                <summary>Last output</summary>
                <pre className="code">{r.last_output}</pre>
              </details>
            )}
            {history.length > 0 && (
              <>
                <GroupHead>History</GroupHead>
                {history.slice(0, 10).map((h, i) => (
                  <details key={i} className="card">
                    <summary>
                      <Chip
                        tone={
                          h.status === "ok"
                            ? "ok"
                            : h.status === "error"
                              ? "err"
                              : "muted"
                        }
                      >
                        {h.status ?? "run"}
                      </Chip>{" "}
                      {relTime(h.ts_ms ?? h.started_ms)}
                    </summary>
                    <pre className="code">{h.error || h.output || "—"}</pre>
                  </details>
                ))}
              </>
            )}
          </div>
        )}
      </Scroll>
      <Sheet open={del} onClose={() => setDel(false)} title="Delete routine?">
        <HoldButton
          label="Hold to delete"
          holding="Keep holding…"
          className="danger block"
          onConfirm={remove}
        />
      </Sheet>
    </>
  );
}

// ── Devices ────────────────────────────────────────────────────────────────

function DevicesView() {
  const { server, toast } = useStore();
  const { data, error, loading, refresh } = useAsync(
    () => api.listDevices(),
    [],
  );
  const [revoke, setRevoke] = useState<string | null>(null);
  const readOnly = error?.toLowerCase().includes("not found") || false;
  const list = data ?? [];
  const doRevoke = async (id: string) => {
    setRevoke(null);
    try {
      await api.revokeDevice(id);
      toast("Device revoked.", "success");
      refresh();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };
  return (
    <>
      <SubHeader title="Devices" onBack={() => back("/more")} />
      <Scroll onRefresh={refresh}>
        {error && !readOnly && <Banner kind="error">{error}</Banner>}
        {readOnly && (
          <Banner kind="warn">
            This server doesn't list devices yet. Manage them in desktop
            Settings → Mobile.
          </Banner>
        )}
        {loading ? (
          <Skeleton rows={2} />
        ) : (
          <div className="list">
            {readOnly && (
              <Row
                leading={<Icon name="phone" size={20} className="accent" />}
                title="This device"
                sub={server.device_id ? `id ${server.device_id}` : "paired"}
              />
            )}
            {list.map((d) => {
              const me = d.current || d.id === server.device_id;
              return (
                <Row
                  key={d.id}
                  leading={
                    <Icon
                      name="phone"
                      size={20}
                      className={me ? "accent" : "muted"}
                    />
                  }
                  title={`${d.name}${me ? " (this device)" : ""}`}
                  sub={`${d.last_seen_ms ? `seen ${relTime(d.last_seen_ms)}` : ""}${d.created_ms ? ` · paired ${relTime(d.created_ms)}` : ""}`}
                  right={
                    !me ? (
                      <button
                        className="linkbtn err"
                        onClick={() => setRevoke(d.id)}
                      >
                        Revoke
                      </button>
                    ) : undefined
                  }
                />
              );
            })}
          </div>
        )}
      </Scroll>
      <Sheet
        open={!!revoke}
        onClose={() => setRevoke(null)}
        title="Revoke this device?"
      >
        <p className="muted">
          It will be signed out immediately and must pair again.
        </p>
        <HoldButton
          label="Hold to revoke"
          holding="Keep holding…"
          className="danger block"
          onConfirm={() => revoke && doRevoke(revoke)}
        />
      </Sheet>
    </>
  );
}

// ── About / install / import ───────────────────────────────────────────────

function AboutView() {
  const { caps, server, mode, connection } = useStore();
  return (
    <>
      <SubHeader title="Server" onBack={() => back("/more")} />
      <Scroll>
        <div className="list">
          <Row title="Name" sub={server.server_name || "Cortex"} />
          <Row
            title="Version"
            sub={caps?.server_version ?? server.server_version ?? "unknown"}
          />
          <Row
            title="API"
            sub={
              mode === "legacy"
                ? "legacy /api (upgrade the desktop for the full app)"
                : "v2"
            }
          />
          <Row title="Address" sub={server.url || location.origin} />
          <Row title="Connection" sub={connection} />
          <Row
            title="Features"
            sub={caps?.features?.length ? caps.features.join(", ") : "—"}
          />
          <Row
            title="Local agents"
            sub={
              caps?.local_agents?.length ? caps.local_agents.join(", ") : "—"
            }
          />
          <Row
            title="Gateway"
            sub={caps?.gateway ? "configured" : "not configured"}
          />
          <Row title="Device id" sub={server.device_id ?? "—"} />
        </div>
      </Scroll>
    </>
  );
}

function InstallView() {
  return (
    <>
      <SubHeader title="Install on iPhone" onBack={() => back("/more")} />
      <Scroll>
        <div className="pad">
          <ol className="steps">
            <li>
              Tap the <b>Share</b> button in Safari's toolbar (the square with
              an arrow).
            </li>
            <li>
              Scroll down and tap <b>Add to Home Screen</b>.
            </li>
            <li>
              Tap <b>Add</b>. Open Cortex from your Home Screen.
            </li>
            <li>
              In Cortex, go to <b>More → Notifications</b> and turn on push.
            </li>
          </ol>
          <p className="muted small">
            Installed apps get full-screen mode, notifications and keep you
            signed in. iOS 16.4+ required for push.
          </p>
        </div>
      </Scroll>
    </>
  );
}

function ImportView() {
  const { toast } = useStore();
  const file = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const pick = async (list: FileList | null) => {
    const f = list?.[0];
    if (!f) return;
    if (f.size > 25 * 1024 * 1024) {
      toast("That export is over 25 MB.", "error");
      return;
    }
    setBusy(true);
    try {
      const text = await f.text();
      const r = await api.importChatFile(text);
      setResult(`Imported ${r.imported} chats (${r.skipped} skipped).`);
      haptic("success");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
      if (file.current) file.current.value = "";
    }
  };
  return (
    <>
      <SubHeader title="Import chat history" onBack={() => back("/more")} />
      <Scroll>
        <div className="pad">
          <p className="muted">
            Pick a <b>conversations.json</b> from a Claude.ai or ChatGPT data
            export, or any generic JSON transcript. Imported chats show up in
            Chats and are resumable.
          </p>
          {result && <Banner kind="success">{result}</Banner>}
          <input
            ref={file}
            type="file"
            accept=".json,application/json,.txt"
            hidden
            onChange={(e) => pick(e.target.files)}
          />
          <button
            className="btn primary block"
            disabled={busy}
            onClick={() => file.current?.click()}
          >
            {busy ? "Importing…" : "Choose file"}
          </button>
          <p className="muted small">
            Tip: on iPhone, exports land in Files → Downloads.
          </p>
        </div>
      </Scroll>
    </>
  );
}
