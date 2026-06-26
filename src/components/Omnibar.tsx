import { useCallback, useEffect, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  chatSend,
  subscribeToSession,
  type AgentEventEnvelope,
} from "@/lib/cortex-bridge";
import {
  extractPlainText,
  renderTextToHTML,
} from "@/lib/token-pills";

/**
 * Raycast-style floating launcher.
 *
 * Lives in its own Tauri window (label "omnibar") and is toggled with
 * Ctrl+Shift+Space. Streams responses inline below the input. ESC hides the
 * window; Enter sends through the same `chat_send` path the main chat uses.
 *
 * Intentionally minimal: no history, no sidebars, no tool cards. If you need
 * those, open the main window.
 *
 * The input is a contenteditable div so we can render `@file:`, `#snippet:`,
 * and `/cmd` tokens as colored pills inline. See `lib/token-pills.ts`.
 */
export function Omnibar() {
  const [response, setResponse] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [empty, setEmpty] = useState(true);
  const editorRef = useRef<HTMLDivElement>(null);
  // We don't store the typed text in React state — that would force us to
  // overwrite the DOM on every keystroke and lose the caret. Instead the
  // contenteditable IS the source of truth; we read it on send.
  // A fresh session per launch keeps the omnibar stateless — it's a quick-ask
  // surface, not a chat. Stored in a ref so streaming callbacks see the
  // session that the send actually used.
  const sessionIdRef = useRef<string>(`omnibar-${crypto.randomUUID()}`);

  /**
   * Read the editor's plain text (chips reversed back into `@file:foo` form),
   * re-render the chip HTML, and restore the caret to the same character
   * offset. This is the heart of the pill-rendering trick.
   */
  const rerenderPills = useCallback(() => {
    const el = editorRef.current;
    if (!el) return;
    const plain = extractPlainText(el.innerHTML);
    setEmpty(plain.length === 0);
    // Save caret position as an offset into the plain text.
    const caretOffset = readCaretOffset(el);
    const nextHTML = renderTextToHTML(plain);
    if (el.innerHTML === nextHTML) return;
    el.innerHTML = nextHTML;
    writeCaretOffset(el, caretOffset);
  }, []);

  // Refocus the editor every time the window becomes visible.
  useEffect(() => {
    const win = getCurrentWindow();
    const focusEditor = () => {
      const el = editorRef.current;
      if (!el) return;
      el.focus();
      // Place caret at end.
      const range = document.createRange();
      range.selectNodeContents(el);
      range.collapse(false);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
    };
    const unlistenPromise = win.onFocusChanged(({ payload: focused }) => {
      if (focused) focusEditor();
    });
    focusEditor();
    return () => {
      void unlistenPromise.then((u) => u());
    };
  }, []);

  // Subscribe to streaming events for the current session. Re-subscribe when
  // the sessionId rotates (after a successful send completes).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;
    const sid = sessionIdRef.current;
    subscribeToSession(sid, (env: AgentEventEnvelope) => {
      if (!mounted) return;
      const evt = env.event;
      if (!evt) return;
      switch (evt.type) {
        case "token":
          setResponse((prev) => prev + evt.delta);
          break;
        case "error":
          setError(evt.message);
          setSending(false);
          break;
        case "done":
          setSending(false);
          break;
        default:
          break;
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      mounted = false;
      unlisten?.();
    };
  }, []);

  async function hide() {
    try {
      await getCurrentWindow().hide();
    } catch {
      /* ignore — closing during dev reload */
    }
  }

  async function send() {
    const el = editorRef.current;
    if (!el) return;
    const message = extractPlainText(el.innerHTML).trim();
    if (!message || sending) return;
    setError(null);
    setResponse("");
    setSending(true);
    el.innerHTML = "";
    setEmpty(true);
    try {
      await chatSend({
        sessionId: sessionIdRef.current,
        message,
      });
    } catch (e) {
      setError(humanizeError(e));
      setSending(false);
    }
  }

  return (
    <div className="omnibar" role="dialog" aria-label="Cortex omnibar">
      <div className="omnibar-input-wrap">
        {empty && (
          <div className="omnibar-placeholder" aria-hidden="true">
            {sending ? "thinking…" : "Ask Cortex…"}
          </div>
        )}
        <div
          ref={editorRef}
          className="omnibar-input omnibar-editable"
          role="textbox"
          aria-multiline="false"
          contentEditable
          suppressContentEditableWarning
          spellCheck={false}
          onInput={rerenderPills}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              void hide();
            } else if (e.key === "Enter") {
              // Single-line input — Enter sends, Shift+Enter is ignored.
              e.preventDefault();
              void send();
            }
          }}
          onPaste={(e) => {
            // Force plain-text paste so users can't dump rich HTML into
            // the editor (which would break the pill regeneration loop).
            e.preventDefault();
            const text = e.clipboardData.getData("text/plain");
            document.execCommand("insertText", false, text);
          }}
        />
      </div>
      {(response || error) && (
        <div className={`omnibar-result ${error ? "omnibar-result-error" : ""}`}>
          {error ?? response}
        </div>
      )}
    </div>
  );
}

/**
 * Return the caret offset into the editor's plain-text representation. A
 * chip counts as the length of its canonical `data-token` string so the
 * offset survives a re-render.
 */
function readCaretOffset(root: HTMLElement): number {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return 0;
  const range = sel.getRangeAt(0);
  if (!root.contains(range.endContainer)) return 0;
  let offset = 0;
  const walk = (node: Node): boolean => {
    if (node === range.endContainer) {
      if (node.nodeType === Node.TEXT_NODE) {
        offset += range.endOffset;
      } else {
        // Element endContainer (caret between children) — count children up to endOffset.
        for (let i = 0; i < range.endOffset; i++) {
          const child = node.childNodes[i];
          if (child) offset += plainLength(child);
        }
      }
      return true;
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      const tok = el.getAttribute("data-token");
      if (tok) {
        offset += tok.length;
        return false;
      }
      for (const child of Array.from(node.childNodes)) {
        if (walk(child)) return true;
      }
      return false;
    }
    if (node.nodeType === Node.TEXT_NODE) {
      offset += (node.textContent ?? "").length;
    }
    return false;
  };
  for (const child of Array.from(root.childNodes)) {
    if (walk(child)) break;
  }
  return offset;
}

function plainLength(node: Node): number {
  if (node.nodeType === Node.TEXT_NODE) return (node.textContent ?? "").length;
  if (node.nodeType === Node.ELEMENT_NODE) {
    const el = node as HTMLElement;
    const tok = el.getAttribute("data-token");
    if (tok) return tok.length;
    let n = 0;
    for (const c of Array.from(el.childNodes)) n += plainLength(c);
    return n;
  }
  return 0;
}

/**
 * Restore the caret to a given offset into the plain-text representation
 * after a re-render. If the offset overshoots the new content (e.g. user
 * just typed the closing char of a token and we collapsed it into a chip)
 * we clamp to the end.
 */
function writeCaretOffset(root: HTMLElement, target: number) {
  let remaining = target;
  let placed: { node: Node; offset: number } | null = null;
  const walk = (node: Node) => {
    if (placed) return;
    if (node.nodeType === Node.TEXT_NODE) {
      const len = (node.textContent ?? "").length;
      if (remaining <= len) {
        placed = { node, offset: remaining };
        return;
      }
      remaining -= len;
      return;
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      const tok = el.getAttribute("data-token");
      if (tok) {
        const len = tok.length;
        if (remaining <= len) {
          // Park the caret immediately after the chip — entering mid-chip is
          // disallowed by contenteditable=false on the chip itself.
          const parent = el.parentNode;
          if (parent) {
            const idx = Array.prototype.indexOf.call(parent.childNodes, el) + 1;
            placed = { node: parent, offset: idx };
          }
          return;
        }
        remaining -= len;
        return;
      }
      for (const child of Array.from(el.childNodes)) walk(child);
    }
  };
  for (const child of Array.from(root.childNodes)) walk(child);
  const sel = window.getSelection();
  if (!sel) return;
  const range = document.createRange();
  if (placed) {
    range.setStart((placed as { node: Node; offset: number }).node, (placed as { node: Node; offset: number }).offset);
  } else {
    range.selectNodeContents(root);
    range.collapse(false);
  }
  range.collapse(true);
  sel.removeAllRanges();
  sel.addRange(range);
}
