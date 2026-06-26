import { useRef, useState } from "react";
import { importChatFile, importChatPull } from "../lib/api";
import type { ImportProvider, ImportResult } from "../lib/types";

/** Conservative cap so a giant export doesn't lock up the WebView while the
 *  FileReader slurps it into memory. The backend still parses tolerantly. */
const MAX_FILE_BYTES = 25 * 1024 * 1024; // 25 MB

type Status =
  | { kind: "idle" }
  | { kind: "busy"; what: string }
  | { kind: "ok"; result: ImportResult }
  | { kind: "err"; message: string };

/** Import external AI chat history (Claude.ai / ChatGPT / generic JSON exports)
 *  into Cortex as resumable sessions. On success the caller is notified so the
 *  Recent tab can refresh and the imported chats appear. */
export default function ImportView({
  onImported,
}: {
  onImported?: (result: ImportResult) => void;
}) {
  const fileInput = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const [pasted, setPasted] = useState("");

  const [provider, setProvider] = useState<ImportProvider>("claude");
  const [token, setToken] = useState("");

  const busy = status.kind === "busy";

  const finish = (result: ImportResult) => {
    setStatus({ kind: "ok", result });
    onImported?.(result);
  };

  const run = async (what: string, fn: () => Promise<ImportResult>) => {
    setStatus({ kind: "busy", what });
    try {
      finish(await fn());
    } catch (e) {
      setStatus({ kind: "err", message: e instanceof Error ? e.message : String(e) });
    }
  };

  const onFile = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    // Reset so picking the same file again re-fires `change`.
    e.target.value = "";
    if (!file) return;
    if (file.size > MAX_FILE_BYTES) {
      setStatus({
        kind: "err",
        message: `File is ${(file.size / 1024 / 1024).toFixed(1)} MB — too large (max ${MAX_FILE_BYTES / 1024 / 1024} MB). Try splitting the export.`,
      });
      return;
    }
    setStatus({ kind: "busy", what: `Reading ${file.name}…` });
    const reader = new FileReader();
    reader.onerror = () =>
      setStatus({ kind: "err", message: `Could not read ${file.name}.` });
    reader.onload = () => {
      const content = String(reader.result ?? "");
      void run(`Importing ${file.name}…`, () => importChatFile(content, "auto"));
    };
    reader.readAsText(file);
  };

  const onPaste = () => {
    const content = pasted.trim();
    if (!content || busy) return;
    void run("Importing pasted JSON…", () => importChatFile(content, "auto"));
  };

  const onPull = () => {
    const t = token.trim();
    if (!t || busy) return;
    void run(`Pulling from ${provider}…`, () => importChatPull(provider, t));
  };

  return (
    <div className="scroll">
      <div className="pad">
        <div className="import-intro faint">
          Bring your Claude.ai or ChatGPT history into Cortex. Imported chats
          become resumable, searchable sessions and show up under Recent.
        </div>

        {/* Status banner */}
        {status.kind === "busy" && (
          <div className="banner import-status">
            <span className="spin" /> {status.what}
          </div>
        )}
        {status.kind === "ok" && (
          <div className="banner import-status ok">
            ✓ Imported {status.result.imported}{" "}
            {status.result.imported === 1 ? "conversation" : "conversations"}
            {status.result.skipped > 0 && `, skipped ${status.result.skipped}`}.
          </div>
        )}
        {status.kind === "err" && (
          <div className="banner err">{status.message}</div>
        )}

        {/* 1 — Import from file */}
        <section className="import-card">
          <h3>Import from file</h3>
          <p className="faint">
            Select a chat-export <code>.json</code> from Claude.ai, ChatGPT, or a
            generic export.
          </p>
          <input
            ref={fileInput}
            type="file"
            accept=".json,application/json"
            hidden
            onChange={onFile}
          />
          <button
            className="btn primary"
            style={{ width: "100%" }}
            disabled={busy}
            onClick={() => fileInput.current?.click()}
          >
            Choose file…
          </button>
        </section>

        {/* 2 — Paste JSON */}
        <section className="import-card">
          <h3>Paste JSON</h3>
          <p className="faint">
            No file handy? Paste the raw export JSON here instead.
          </p>
          <div className="field">
            <textarea
              value={pasted}
              onChange={(e) => setPasted(e.target.value)}
              placeholder='{"conversations": [ … ]}'
              disabled={busy}
              spellCheck={false}
            />
          </div>
          <button
            className="btn"
            style={{ width: "100%" }}
            disabled={busy || !pasted.trim()}
            onClick={onPaste}
          >
            Import pasted JSON
          </button>
        </section>

        {/* 3 — Experimental pull */}
        <section className="import-card experimental">
          <h3>
            <span className="warn-pill">⚠️ Experimental / unofficial</span>
          </h3>
          <p className="faint">
            Pull directly from your account using a session token. This uses
            unofficial endpoints that are fragile and may break or fail at any
            time. Your token is sent once to import and is never stored or logged.
          </p>

          <div className="field">
            <label>Provider</label>
            <div className="segmented">
              <button
                type="button"
                className={provider === "claude" ? "active" : ""}
                disabled={busy}
                onClick={() => setProvider("claude")}
              >
                Claude.ai
              </button>
              <button
                type="button"
                className={provider === "chatgpt" ? "active" : ""}
                disabled={busy}
                onClick={() => setProvider("chatgpt")}
              >
                ChatGPT
              </button>
            </div>
          </div>

          <div className="field">
            <label>Session token</label>
            <input
              type="password"
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder={provider === "claude" ? "sessionKey value" : "accessToken value"}
              disabled={busy}
            />
            <div className="faint import-hint">
              {provider === "claude" ? (
                <>
                  Claude.ai: copy the <code>sessionKey</code> cookie value
                  (DevTools → Application → Cookies → claude.ai).
                </>
              ) : (
                <>
                  ChatGPT: open{" "}
                  <code>chatgpt.com/api/auth/session</code> while signed in and
                  copy the <code>accessToken</code> value.
                </>
              )}
            </div>
          </div>

          <button
            className="btn"
            style={{ width: "100%" }}
            disabled={busy || !token.trim()}
            onClick={onPull}
          >
            Pull from {provider === "claude" ? "Claude.ai" : "ChatGPT"}
          </button>
        </section>
      </div>
    </div>
  );
}
