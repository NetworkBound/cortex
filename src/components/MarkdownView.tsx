import { memo, useState, isValidElement, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { open as openExternal } from "@tauri-apps/plugin-shell";

interface Props {
  source: string;
}

// Pull the raw fence language off the inner `<code class="language-xxx">`
// that ReactMarkdown hands to the `pre` renderer. Returns "" when the fence
// carried no language hint (an untagged ```block```).
function rawLangFromChildren(children: ReactNode): string {
  const code = Array.isArray(children) ? children[0] : children;
  if (!isValidElement(code)) return "";
  const className = (code.props as { className?: string }).className ?? "";
  const m = /\blanguage-([\w+-]+)/.exec(className);
  return m ? m[1].toLowerCase() : "";
}

// Human-facing label for the code header; "" keeps just the copy affordance
// without inventing a language.
function langFromChildren(children: ReactNode): string {
  const raw = rawLangFromChildren(children);
  return raw ? (LANG_LABELS[raw] ?? raw) : "";
}

// Friendly display names for the common fences; anything else shows verbatim.
const LANG_LABELS: Record<string, string> = {
  ts: "TypeScript",
  tsx: "TSX",
  js: "JavaScript",
  jsx: "JSX",
  py: "Python",
  rs: "Rust",
  sh: "Shell",
  bash: "Shell",
  zsh: "Shell",
  json: "JSON",
  yaml: "YAML",
  yml: "YAML",
  toml: "TOML",
  html: "HTML",
  css: "CSS",
  md: "Markdown",
  sql: "SQL",
  go: "Go",
  c: "C",
  cpp: "C++",
  rb: "Ruby",
  java: "Java",
  diff: "Diff",
};

// Read a `<code>` child as a plain string so the Copy button copies exactly
// what was rendered (children may be a string, an array of strings, or React
// nodes when highlight.js has inserted spans).
function nodeToText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(nodeToText).join("");
  if (typeof node === "object" && "props" in node) {
    return nodeToText((node as { props: { children?: ReactNode } }).props.children);
  }
  return "";
}

// One `- [ ]` / `- [x]` line per task; mirrors the backend scanner's parse
// (commands/focus_chain.rs) so the chip shows exactly what landed in the
// FocusChain panel. Non-checklist lines are skipped; zero matches makes the
// caller fall back to a plain code block.
function parseFocusItems(text: string): Array<{ title: string; done: boolean }> {
  const items: Array<{ title: string; done: boolean }> = [];
  for (const line of text.split("\n")) {
    const m = /^\s*(?:[-*]\s*)?\[( |x|X)\]\s*(\S.*)$/.exec(line);
    if (m) items.push({ title: m[2].trim(), done: m[1] !== " " });
  }
  return items;
}

// The agent's ```focus-chain fence rendered as the checklist it IS instead of
// raw code — the live copy of this state lives in the FocusChain activity
// tab (the backend re-emits the block as an `update_focus_chain` tool call).
function FocusChainBlock({ items }: { items: Array<{ title: string; done: boolean }> }) {
  const done = items.filter((t) => t.done).length;
  return (
    <div className="md-focus-chain">
      <div className="md-focus-chain-head">
        <span className="md-focus-chain-label">Focus chain</span>
        <span className="md-focus-chain-count">
          {done}/{items.length} done
        </span>
      </div>
      <ul className="md-focus-chain-list">
        {items.map((t, i) => (
          <li key={i} className={`md-focus-chain-item ${t.done ? "done" : ""}`}>
            <input type="checkbox" checked={t.done} readOnly tabIndex={-1} aria-hidden />
            <span>{t.title}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function CopyButton({ getText }: { getText: () => string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="md-code-copy"
      aria-label="Copy code"
      onClick={() => {
        navigator.clipboard
          .writeText(getText())
          .then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1200);
          })
          .catch(() => { /* clipboard unavailable; ignore */ });
      }}
    >
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

// Memoized: re-renders only when `source` changes. During token streaming this
// stops every prior message's markdown from being re-parsed + re-highlighted on
// each token (the dominant chat-render cost) — see ChatPane MessageView.
export const MarkdownView = memo(function MarkdownView({ source }: Props) {
  return (
    <div className="md-view">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{
          // ReactMarkdown v9 hands fenced code through as `<pre><code>`. We
          // wrap it in a `.md-code` figure with a persistent header bar — the
          // language label (left) + a Copy button (right) — the Claude.ai /
          // ChatGPT / Cursor / GitHub pattern. Inline `<code>` stays a bare
          // element styled via `.md-view code`.
          pre({ children, ...rest }) {
            // Extract the inner code text so the Copy button copies the raw
            // source (without any highlight.js span markup).
            const getText = () => nodeToText(children);
            // Focus-chain fences render as the checklist they encode, not as
            // code (falls through to a plain block when nothing parses).
            if (rawLangFromChildren(children) === "focus-chain") {
              const items = parseFocusItems(nodeToText(children));
              if (items.length > 0) return <FocusChainBlock items={items} />;
            }
            const lang = langFromChildren(children);
            return (
              <div className="md-code">
                <div className="md-code-head">
                  <span className="md-code-lang">{lang}</span>
                  <CopyButton getText={getText} />
                </div>
                <pre {...rest}>{children}</pre>
              </div>
            );
          },
          a({ href, children, ...rest }) {
            return (
              <a
                {...rest}
                href={href}
                onClick={(e) => {
                  e.preventDefault();
                  if (href) void openExternal(href).catch(() => { /* ignore */ });
                }}
              >
                {children}
              </a>
            );
          },
          img({ src, alt, ...rest }) {
            return (
              <img
                {...rest}
                src={src}
                alt={alt ?? ""}
                loading="lazy"
                style={{ maxWidth: "100%", height: "auto" }}
              />
            );
          },
        }}
      >
        {source}
      </ReactMarkdown>
    </div>
  );
});
