// Chat composer: auto-growing textarea, Enter to send / Shift+Enter newline,
// @-token and /slash chips, image attachments, Stop while a run is active.

import { useEffect, useRef, useState } from "react";
import Icon from "./Icon";
import { AT_TOKENS, SLASH_COMMANDS, activeToken } from "../lib/composerVocab";
import { haptic } from "../lib/native";
import type { Attachment } from "../lib/types";

const MAX_FILES = 4;
const MAX_BYTES = 8 * 1024 * 1024;

export type LocalCommand = "clear" | "stop" | "help";

export function Composer({
  running,
  disabled,
  placeholder,
  allowAttachments,
  onSend,
  onStop,
  onLocal,
  notify,
  draftKey,
}: {
  running: boolean;
  disabled?: boolean;
  placeholder?: string;
  allowAttachments: boolean;
  onSend: (text: string, attachments: Attachment[]) => void;
  onStop: () => void;
  onLocal: (cmd: LocalCommand) => void;
  notify: (text: string) => void;
  /** Persist the draft per thread. */
  draftKey?: string;
}) {
  const [text, setText] = useState("");
  const [files, setFiles] = useState<Attachment[]>([]);
  const [caret, setCaret] = useState(0);
  const ta = useRef<HTMLTextAreaElement | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  // Draft persistence (per thread).
  useEffect(() => {
    if (!draftKey) return;
    try {
      setText(sessionStorage.getItem(`draft:${draftKey}`) ?? "");
    } catch {
      /* ignore */
    }
  }, [draftKey]);
  useEffect(() => {
    if (!draftKey) return;
    try {
      if (text) sessionStorage.setItem(`draft:${draftKey}`, text);
      else sessionStorage.removeItem(`draft:${draftKey}`);
    } catch {
      /* ignore */
    }
  }, [text, draftKey]);

  // Auto-grow.
  useEffect(() => {
    const el = ta.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);

  const tok = activeToken(text, caret);
  const suggestions =
    tok?.kind === "slash"
      ? SLASH_COMMANDS.filter((c) => c.name.startsWith(tok.query)).slice(0, 8)
      : tok?.kind === "at"
        ? AT_TOKENS.filter((t) =>
            t.insert.slice(1).toLowerCase().startsWith(tok.query),
          ).slice(0, 8)
        : [];

  const insert = (value: string) => {
    if (!tok) return;
    const before = text.slice(0, tok.start);
    const after = text.slice(caret);
    const trailing = value.endsWith(":") ? "" : " ";
    const next = `${before}${value}${trailing}${after}`;
    setText(next);
    const pos = before.length + value.length + trailing.length;
    requestAnimationFrame(() => {
      ta.current?.focus();
      ta.current?.setSelectionRange(pos, pos);
      setCaret(pos);
    });
    haptic("selection");
  };

  const submit = () => {
    const t = text.trim();
    if (!t && files.length === 0) return;
    if (disabled) return;
    // Slash commands.
    const m = /^\/([a-z-]+)\b/i.exec(t);
    if (m) {
      const name = m[1].toLowerCase();
      const cmd = SLASH_COMMANDS.find((c) => c.name === name);
      if (cmd?.local) {
        setText("");
        onLocal(cmd.name as LocalCommand);
        return;
      }
      if (cmd) {
        notify(`/${cmd.name} runs on the desktop app only.`);
        return;
      }
    }
    onSend(t, files);
    setText("");
    setFiles([]);
    haptic("light");
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  const pick = async (list: FileList | null) => {
    if (!list) return;
    const next: Attachment[] = [...files];
    for (const f of Array.from(list)) {
      if (next.length >= MAX_FILES) {
        notify(`Up to ${MAX_FILES} images per message.`);
        break;
      }
      if (f.size > MAX_BYTES) {
        notify(`${f.name} is over 8 MB.`);
        continue;
      }
      const data = await new Promise<string>((res, rej) => {
        const r = new FileReader();
        r.onload = () => res(String(r.result).split(",")[1] ?? "");
        r.onerror = () => rej(r.error);
        r.readAsDataURL(f);
      }).catch(() => "");
      if (data) {
        next.push({
          name: f.name,
          mime: f.type || "image/*",
          data_base64: data,
        });
      }
    }
    setFiles(next);
    if (fileInput.current) fileInput.current.value = "";
  };

  return (
    <div className="composer">
      {suggestions.length > 0 && (
        <div className="chips" role="listbox">
          {suggestions.map((s) =>
            "insert" in s ? (
              <button
                key={s.insert}
                className="chip-btn"
                onClick={() => insert(s.insert)}
                title={s.hint}
              >
                {s.label}
              </button>
            ) : (
              <button
                key={s.name}
                className={`chip-btn ${s.local ? "" : "dim"}`}
                onClick={() => insert(`/${s.name}`)}
                title={s.description}
              >
                /{s.name}
              </button>
            ),
          )}
        </div>
      )}
      {files.length > 0 && (
        <div className="attach-strip">
          {files.map((f, i) => (
            <span key={i} className="attach-thumb">
              <img
                src={`data:${f.mime};base64,${f.data_base64}`}
                alt={f.name}
              />
              <button
                aria-label={`Remove ${f.name}`}
                onClick={() => setFiles(files.filter((_, j) => j !== i))}
              >
                <Icon name="x" size={12} />
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="composer-row">
        {allowAttachments && (
          <>
            <button
              className="iconbtn"
              aria-label="Attach image"
              disabled={disabled}
              onClick={() => fileInput.current?.click()}
            >
              <Icon name="attach" size={20} />
            </button>
            <input
              ref={fileInput}
              type="file"
              accept="image/*"
              multiple
              hidden
              onChange={(e) => pick(e.target.files)}
            />
          </>
        )}
        <textarea
          ref={ta}
          value={text}
          rows={1}
          placeholder={placeholder ?? "Message Cortex…"}
          disabled={disabled}
          enterKeyHint="send"
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart ?? e.target.value.length);
          }}
          onSelect={(e) =>
            setCaret((e.target as HTMLTextAreaElement).selectionStart ?? 0)
          }
          onKeyDown={onKey}
        />
        {running ? (
          <button className="send stop" onClick={onStop} aria-label="Stop">
            <Icon name="stop" size={18} />
          </button>
        ) : (
          <button
            className="send"
            onClick={submit}
            disabled={disabled || (!text.trim() && files.length === 0)}
            aria-label="Send"
          >
            <Icon name="send" size={20} />
          </button>
        )}
      </div>
    </div>
  );
}
