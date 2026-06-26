import ReactMarkdown from "react-markdown";

/** Message-body markdown renderer with the app's `.md` styling. */
export default function Markdown({ children }: { children: string }) {
  return (
    <div className="md">
      <ReactMarkdown>{children}</ReactMarkdown>
    </div>
  );
}
