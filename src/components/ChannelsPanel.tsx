import { useEffect, useMemo, useRef, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  type Channel,
  type ChannelMessage,
  type MemberSpec,
  colorForAuthor,
  createChannel,
  deleteChannel,
  extractMentions,
  getChannel,
  listChannels,
  postMessage,
} from "@/lib/channels";
import { listRoles, type Role } from "@/lib/roles";
import { MarkdownView } from "./MarkdownView";
import { pushToast } from "@/lib/toast";
import "@/styles/channels.css";

/**
 * Live per-agent progress emitted by the backend `post_message` flow over the
 * channel-scoped `channels:progress:<channelId>` event. Mirrors the Rust
 * `ChannelProgress` struct. `text` carries the token chunk on `delta` and the
 * final reply on `done`/`error`.
 */
interface ChannelProgress {
  role: string;
  status: "start" | "delta" | "done" | "error";
  text?: string | null;
}

/** One row in the in-flight progress strip (status only — reply text streams
 *  into the transcript as a live bubble instead). */
type AgentProgress = {
  role: string;
  status: "start" | "done" | "error";
};

/** An in-flight agent reply growing in the transcript as deltas arrive. */
type LiveReply = {
  role: string;
  content: string;
  status: "streaming" | "done" | "error";
};

/**
 * Multi-agent Channels — Open WebUI / Slack-style persistent rooms where the
 * user and one-or-more `~/.cortex/roles/*.yaml` personas coexist. `@role-name`
 * in the composer summons that role via the gateway and inlines its reply.
 *
 * Two-pane layout: left sidebar lists channels and exposes a "New channel"
 * affordance, right pane shows the active channel's transcript + composer.
 * Auto-scrolls to bottom on new messages.
 */

type Pending = { id: string; channelId: string };

export function ChannelsPanel() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [active, setActive] = useState<Channel | null>(null);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState<Pending | null>(null);
  // Per-agent live progress for the in-flight send, keyed by role order.
  const [progress, setProgress] = useState<AgentProgress[]>([]);
  // Optimistic transcript state for the in-flight send: the user's own post
  // (shown immediately, before the backend persists it) and one streaming
  // bubble per summoned role that grows as `delta` events land.
  const [liveUser, setLiveUser] = useState<{ content: string; ts: number } | null>(null);
  const [liveReplies, setLiveReplies] = useState<LiveReply[]>([]);
  const [showCreate, setShowCreate] = useState(false);
  const [roles, setRoles] = useState<Role[]>([]);
  const [error, setError] = useState<string | null>(null);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  // Holds the active progress-event unlisten so we can tear it down whether the
  // send resolves, errors, or the component unmounts mid-flight.
  const progressUnlistenRef = useRef<UnlistenFn | null>(null);

  // Initial load + role catalog for the composer autocomplete / new-channel
  // member picker. Roles are static-ish so we fetch them once. Honours any
  // pending preselect dropped by `/channel <name>` before the panel mounted.
  useEffect(() => {
    let cancelled = false;
    void Promise.all([listChannels(), listRoles()])
      .then(([cs, rs]) => {
        if (cancelled) return;
        setChannels(cs);
        setRoles(rs);
        const pre = consumeChannelsPreselect();
        const target = pre && cs.find((c) => c.id === pre)?.id;
        if (target) setActiveId(target);
        else if (cs.length > 0 && !activeId) setActiveId(cs[0].id);
      })
      .catch((e) => !cancelled && setError(humanizeError(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Refetch the active channel whenever the selection changes. The list view
  // only has the metadata we got at boot, so re-pull to pick up any backend
  // appends from other windows.
  useEffect(() => {
    if (!activeId) {
      setActive(null);
      return;
    }
    let cancelled = false;
    void getChannel(activeId)
      .then((c) => !cancelled && setActive(c))
      .catch((e) => !cancelled && setError(humanizeError(e)));
    return () => {
      cancelled = true;
    };
  }, [activeId]);

  // Auto-scroll on new messages AND as streamed reply text grows. Run after
  // paint so the layout is settled.
  const liveLen = liveReplies.reduce((n, r) => n + r.content.length, 0);
  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [active?.messages.length, active?.id, liveUser, liveLen]);

  // Tear down any dangling progress listener if we unmount mid-send.
  useEffect(() => {
    return () => {
      progressUnlistenRef.current?.();
      progressUnlistenRef.current = null;
    };
  }, []);

  const mentioned = useMemo(() => extractMentions(draft), [draft]);
  const memberSet = useMemo(
    () => new Set(active?.members.filter((m) => m.kind === "agent_role").map((m) => m.id) ?? []),
    [active],
  );
  const unknownMentions = mentioned.filter((m) => !memberSet.has(m));

  async function send() {
    if (!active || sending) return;
    const content = draft.trim();
    if (!content) return;
    const channelId = active.id;
    const pending: Pending = { id: crypto.randomUUID(), channelId };
    setSending(pending);
    setDraft("");
    // Show the user's post in the transcript immediately — the backend won't
    // return the persisted copy until every summoned role has finished.
    setLiveUser({ content, ts: Date.now() });
    setLiveReplies([]);

    // Seed the progress list from the draft's mentions so the user sees the
    // expected roster immediately (queued state) even before the first event.
    // The backend emits start → delta* → done(or error) per role; we reconcile
    // by role name. If NO events arrive (older backend), this stays "queued"
    // and the final return below still resolves the UI — we never hang.
    const expected = extractMentions(content);
    setProgress(expected.map((role) => ({ role, status: "start" as const })));

    // Subscribe before invoking so we don't miss the first emit. Stash the
    // unlisten in a ref so the finally-block (and unmount) can tear it down.
    try {
      progressUnlistenRef.current?.();
      progressUnlistenRef.current = await listen<ChannelProgress>(
        `channels:progress:${channelId}`,
        (evt) => {
          const { role, status, text } = evt.payload;
          if (status === "delta") {
            // Token chunk — grow that role's streaming bubble in place.
            setLiveReplies((cur) => {
              const i = cur.findIndex((r) => r.role === role);
              if (i < 0) return [...cur, { role, content: text ?? "", status: "streaming" }];
              const next = [...cur];
              next[i] = { ...next[i], content: next[i].content + (text ?? "") };
              return next;
            });
            return;
          }
          if (status === "start") {
            // Open an (empty) streaming bubble so the reader sees who's typing.
            setLiveReplies((cur) =>
              cur.some((r) => r.role === role)
                ? cur
                : [...cur, { role, content: "", status: "streaming" }],
            );
          } else {
            // done/error — `text` is the final authoritative reply (it also
            // covers timeout/empty placeholders), so prefer it over the
            // accumulated deltas.
            setLiveReplies((cur) => {
              const i = cur.findIndex((r) => r.role === role);
              const row: LiveReply = {
                role,
                content: text ?? (i >= 0 ? cur[i].content : ""),
                status,
              };
              if (i < 0) return [...cur, row];
              const next = [...cur];
              next[i] = row;
              return next;
            });
          }
          setProgress((cur) => {
            const next = [...cur];
            const i = next.findIndex((p) => p.role === role);
            const row: AgentProgress = { role, status };
            if (i >= 0) next[i] = { ...next[i], ...row };
            else next.push(row);
            return next;
          });
        },
      );
    } catch {
      // Listener wiring failed (e.g. event API unavailable) — degrade to the
      // plain blocking path; the invoke below still resolves the UI.
      progressUnlistenRef.current = null;
    }

    try {
      const appended = await postMessage(channelId, content);
      // Splice the persisted messages locally (no re-fetch needed). React
      // batches this with the live-state clears in `finally`, so the streamed
      // bubbles swap for their persisted twins in a single paint.
      setActive((cur) =>
        cur && cur.id === channelId ? { ...cur, messages: [...cur.messages, ...appended] } : cur,
      );
    } catch (e) {
      setError(humanizeError(e));
      pushToast({ title: "Channel post failed", body: humanizeError(e), kind: "error" });
      // Nothing was persisted — put the text back so the user can retry,
      // unless they've already started a new draft.
      setDraft((d) => (d.trim().length === 0 ? content : d));
    } finally {
      progressUnlistenRef.current?.();
      progressUnlistenRef.current = null;
      setSending(null);
      setProgress([]);
      setLiveUser(null);
      setLiveReplies([]);
    }
  }

  async function handleCreate(name: string, description: string, picked: string[]) {
    const trimmed = name.trim();
    if (!trimmed) {
      pushToast({ title: "Channel name required", body: "Pick a name to create the room.", kind: "warning" });
      return;
    }
    const members: MemberSpec[] = [
      { kind: "user", id: "user", name: "You" },
      ...picked.map((id) => ({ kind: "agent_role" as const, id, name: id })),
    ];
    try {
      const created = await createChannel(trimmed, description, members);
      setChannels((cs) => [created, ...cs]);
      setActiveId(created.id);
      setShowCreate(false);
      pushToast({ title: "Channel created", body: created.name, kind: "success" });
    } catch (e) {
      pushToast({ title: "Create failed", body: humanizeError(e), kind: "error" });
    }
  }

  async function handleDelete(id: string) {
    if (!(await confirmDialog({
      title: "Delete channel?",
      message: "The channel and its transcript will be permanently deleted.",
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    try {
      await deleteChannel(id);
      setChannels((cs) => cs.filter((c) => c.id !== id));
      if (activeId === id) setActiveId(null);
    } catch (e) {
      pushToast({ title: "Delete failed", body: humanizeError(e), kind: "error" });
    }
  }

  return (
    <div className="channels-root">
      <aside className="channels-sidebar">
        <div className="channels-sidebar-head">
          <strong>Channels</strong>
          <button className="channels-new-btn" onClick={() => setShowCreate(true)} title="New channel">
            + New
          </button>
        </div>
        {channels.length === 0 ? (
          !error && <div className="muted channels-empty">No channels yet.</div>
        ) : (
          <ul className="channels-list">
            {channels.map((c) => (
              <li key={c.id}>
                <button
                  className={`channels-row${activeId === c.id ? " active" : ""}`}
                  onClick={() => setActiveId(c.id)}
                >
                  <span className="channels-row-hash">#</span>
                  <span className="channels-row-name">{c.name}</span>
                  <span className="muted channels-row-meta">
                    {c.members.filter((m) => m.kind === "agent_role").length}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
        {error && <div className="channels-error">{error}</div>}
      </aside>

      <section className="channels-main">
        {!active ? (
          !error && (
            <div className="muted channels-empty-main">
              {channels.length === 0 ? "Create a channel to start." : "Pick a channel."}
            </div>
          )
        ) : (
          <>
            <header className="channels-header">
              <div>
                <h3 className="channels-title">#{active.name}</h3>
                {active.description && <div className="muted channels-desc">{active.description}</div>}
              </div>
              <button className="link-btn channels-delete" onClick={() => handleDelete(active.id)}>
                Delete
              </button>
            </header>
            <div className="channels-members">
              {active.members.map((m) => (
                <span
                  key={`${m.kind}-${m.id}`}
                  className="channels-chip"
                  style={{
                    borderColor: m.kind === "agent_role" ? colorForAuthor(m.id) : "var(--border)",
                  }}
                  title={m.kind}
                >
                  {m.kind === "agent_role" ? "@" : ""}
                  {m.name}
                </span>
              ))}
            </div>
            <div ref={scrollerRef} className="channels-transcript">
              {active.messages.length === 0 && !(sending?.channelId === active.id) ? (
                <div className="muted channels-empty-transcript">
                  No messages yet. Try <code>@{active.members.find((m) => m.kind === "agent_role")?.id ?? "role-name"}</code> to summon an agent.
                </div>
              ) : (
                <>
                  {active.messages.map((m) => <Bubble key={m.id} msg={m} />)}
                  {sending?.channelId === active.id && (
                    <>
                      {liveUser && (
                        <Bubble
                          msg={{
                            id: "live-user",
                            author_kind: "user",
                            author_id: "user",
                            content: liveUser.content,
                            ts: liveUser.ts,
                            mentions: [],
                          }}
                        />
                      )}
                      {liveReplies.map((r) => (
                        <LiveBubble key={r.role} reply={r} />
                      ))}
                    </>
                  )}
                </>
              )}
            </div>
            <div className="channels-composer">
              {sending && (
                <ProgressTray progress={progress} />
              )}
              {unknownMentions.length > 0 && (
                <div className="channels-warn">
                  Unknown mention{unknownMentions.length === 1 ? "" : "s"}:{" "}
                  {unknownMentions.map((u) => `@${u}`).join(", ")}
                </div>
              )}
              <textarea
                className="channels-textarea"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    void send();
                  }
                }}
                placeholder="Message #channel — use @role-name to summon an agent (Ctrl+Enter to send)"
                rows={3}
              />
              <div className="channels-composer-foot">
                <span className="muted">{mentioned.length > 0 && `summoning: ${mentioned.map((m) => `@${m}`).join(" ")}`}</span>
                <button
                  className="channels-send-btn"
                  onClick={() => void send()}
                  disabled={sending !== null || draft.trim().length === 0}
                >
                  {sending
                    ? progress.length > 0
                      ? `Sending ${progress.filter((p) => p.status !== "start").length}/${progress.length}…`
                      : "Sending…"
                    : "Send"}
                </button>
              </div>
            </div>
          </>
        )}
      </section>

      {showCreate && (
        <CreateChannelModal roles={roles} onCancel={() => setShowCreate(false)} onCreate={handleCreate} />
      )}
    </div>
  );
}

/**
 * In-flight per-agent status strip. Shows one row per summoned role with a
 * spinner → checkmark / ✕ (reply text streams into the transcript as a live
 * bubble, so this stays compact). When the send mentions no roles (or events
 * haven't arrived yet) we fall back to a generic "Sending…" line so the user
 * always sees forward motion.
 */
function ProgressTray({ progress }: { progress: AgentProgress[] }) {
  if (progress.length === 0) {
    return (
      <div className="channels-progress">
        <div className="channels-progress-row">
          <span className="channels-progress-spinner" aria-hidden />
          <span className="channels-progress-role">Sending…</span>
        </div>
      </div>
    );
  }
  return (
    <div className="channels-progress">
      {progress.map((p) => (
        <div key={p.role} className={`channels-progress-row ${p.status}`}>
          <span className="channels-progress-icon" aria-hidden>
            {p.status === "done" ? "✓" : p.status === "error" ? "✕" : null}
            {p.status === "start" && <span className="channels-progress-spinner" aria-hidden />}
          </span>
          <span className="channels-progress-role" style={{ color: colorForAuthor(p.role) }}>
            @{p.role}
          </span>
        </div>
      ))}
    </div>
  );
}

function Bubble({ msg }: { msg: ChannelMessage }) {
  const isUser = msg.author_kind === "user";
  const isSystem = msg.author_kind === "system";
  const color = isUser ? "var(--accent)" : colorForAuthor(msg.author_id);
  return (
    <div className={`channels-bubble${isUser ? " user" : ""}${isSystem ? " system" : ""}`}>
      <div className="channels-bubble-head">
        <span className="channels-bubble-author" style={{ color }}>
          {isUser ? "You" : isSystem ? "system" : `@${msg.author_id}`}
        </span>
        <span className="muted channels-bubble-ts">{timeShort(msg.ts)}</span>
      </div>
      {isUser ? (
        // The user's own post stays verbatim (same as ChatPane) — only agent /
        // system prose is markdown.
        <div className="channels-bubble-body">{msg.content}</div>
      ) : (
        <div className="channels-bubble-body is-md">
          <MarkdownView source={msg.content} />
        </div>
      )}
    </div>
  );
}

/**
 * A transcript bubble for an agent reply still in flight: empty → animated
 * typing dots; streaming → markdown that grows as deltas land; done/error →
 * final text (kept on screen until the persisted copy replaces it).
 */
function LiveBubble({ reply }: { reply: LiveReply }) {
  return (
    <div className={`channels-bubble live${reply.status === "error" ? " errored" : ""}`}>
      <div className="channels-bubble-head">
        <span className="channels-bubble-author" style={{ color: colorForAuthor(reply.role) }}>
          @{reply.role}
        </span>
        {reply.status === "streaming" && (
          <span className="muted channels-bubble-ts">typing…</span>
        )}
      </div>
      <div className="channels-bubble-body is-md">
        {reply.content ? (
          <MarkdownView source={reply.content} />
        ) : (
          <span className="channels-typing" role="status" aria-label={`@${reply.role} is replying`}>
            <span />
            <span />
            <span />
          </span>
        )}
      </div>
    </div>
  );
}

interface CreateChannelModalProps {
  roles: Role[];
  onCancel: () => void;
  onCreate: (name: string, description: string, members: string[]) => void | Promise<void>;
}

function CreateChannelModal({ roles, onCancel, onCreate }: CreateChannelModalProps) {
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  return (
    <div className="channels-modal-backdrop" onClick={onCancel}>
      <div className="channels-modal" onClick={(e) => e.stopPropagation()}>
        <h3>New channel</h3>
        <label className="channels-modal-label">
          Name
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="eng-standup"
            autoFocus
          />
        </label>
        <label className="channels-modal-label">
          Description
          <input
            value={desc}
            onChange={(e) => setDesc(e.target.value)}
            placeholder="Daily standup, owners, blockers"
          />
        </label>
        <div className="channels-modal-label">Agent members</div>
        {roles.length === 0 ? (
          <div className="muted">No roles found at ~/.cortex/roles/. Create one first.</div>
        ) : (
          <div className="channels-role-grid">
            {roles.map((r) => {
              const on = picked.includes(r.name);
              return (
                <button
                  key={r.name}
                  className={`channels-role-pick${on ? " on" : ""}`}
                  onClick={() =>
                    setPicked((cur) => (on ? cur.filter((n) => n !== r.name) : [...cur, r.name]))
                  }
                  style={{ borderColor: on ? colorForAuthor(r.name) : "var(--border)" }}
                  type="button"
                >
                  @{r.name}
                </button>
              );
            })}
          </div>
        )}
        <div className="channels-modal-foot">
          <button className="link-btn" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="channels-send-btn"
            onClick={() => void onCreate(name, desc, picked)}
            disabled={!name.trim()}
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
}

function timeShort(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// ── Cross-component nudge so /channel can pre-select a target ────────────────
//
// The slash command lives outside the React tree; we expose a tiny module-level
// signal it can flip so the panel picks up the right channel on next mount. No
// global event bus needed — the panel reads `__channelsPreselect` on mount.

let pendingPreselect: string | null = null;
export function setChannelsPreselect(channelId: string | null) {
  pendingPreselect = channelId;
}
export function consumeChannelsPreselect(): string | null {
  const v = pendingPreselect;
  pendingPreselect = null;
  return v;
}
