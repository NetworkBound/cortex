import { useEffect, useMemo, useRef } from "react";
import hljs from "highlight.js/lib/core";
import javascript from "highlight.js/lib/languages/javascript";
import typescript from "highlight.js/lib/languages/typescript";
import rust from "highlight.js/lib/languages/rust";
import python from "highlight.js/lib/languages/python";
import bash from "highlight.js/lib/languages/bash";
import json from "highlight.js/lib/languages/json";
import yaml from "highlight.js/lib/languages/yaml";
import xml from "highlight.js/lib/languages/xml";
import markdown from "highlight.js/lib/languages/markdown";
import css from "highlight.js/lib/languages/css";

hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("python", python);
hljs.registerLanguage("bash", bash);
hljs.registerLanguage("sh", bash);
hljs.registerLanguage("shell", bash);
hljs.registerLanguage("json", json);
hljs.registerLanguage("yaml", yaml);
hljs.registerLanguage("yml", yaml);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("html", xml);
hljs.registerLanguage("markdown", markdown);
hljs.registerLanguage("md", markdown);
hljs.registerLanguage("css", css);

interface Props {
  code: string;
  lang?: string;
}

export function SyntaxBlock({ code, lang }: Props) {
  const ref = useRef<HTMLElement>(null);
  const html = useMemo(() => {
    try {
      if (lang && hljs.getLanguage(lang)) {
        return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
      }
      return hljs.highlightAuto(code, [
        "rust", "typescript", "javascript", "python", "bash", "json", "yaml", "markdown",
      ]).value;
    } catch {
      return escapeHtml(code);
    }
  }, [code, lang]);

  useEffect(() => {
    if (ref.current) ref.current.innerHTML = html;
  }, [html]);

  return (
    <pre className={`code-block lang-${lang ?? "auto"}`}>
      <code ref={ref} />
    </pre>
  );
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

/**
 * Split markdown text into alternating prose / fenced code blocks.
 * Renders fenced blocks via SyntaxBlock; prose as plain text.
 */
export function MarkdownishContent({ text }: { text: string }) {
  const parts = useMemo(() => splitFencedCode(text), [text]);
  return (
    <div className="md-content">
      {parts.map((p, i) =>
        p.kind === "code" ? (
          <SyntaxBlock key={i} code={p.code} lang={p.lang} />
        ) : (
          <span key={i} className="md-prose">{p.text}</span>
        ),
      )}
    </div>
  );
}

type Part =
  | { kind: "text"; text: string }
  | { kind: "code"; code: string; lang: string | undefined };

function splitFencedCode(text: string): Part[] {
  const out: Part[] = [];
  const re = /```([a-zA-Z0-9_+-]*)\n([\s\S]*?)```/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push({ kind: "text", text: text.slice(last, m.index) });
    out.push({ kind: "code", code: m[2], lang: m[1] || undefined });
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last) });
  return out;
}
