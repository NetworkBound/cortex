/**
 * Live markdown preview rendered alongside the CodeMirror buffer in
 * `EditorPane`. Re-renders on every keystroke (the parent passes the
 * live doc through `source`).
 *
 * We deliberately reuse `MarkdownView` so we stay consistent with the
 * existing reader — same code-fence Copy button, same external-link
 * handling, same GFM + highlight.js setup. The wrapper here just adds
 * a scroll container and a small "preview" header so users can see at
 * a glance that the right pane is the rendered view.
 */
import { MarkdownView } from "./MarkdownView";

interface Props {
  /** Current markdown source. Comes from EditorPane's `liveBodyRef`. */
  source: string;
}

export function MarkdownPreview({ source }: Props) {
  return (
    <div className="md-preview">
      <div className="md-preview-head muted">preview</div>
      <div className="md-preview-body">
        <MarkdownView source={source} />
      </div>
    </div>
  );
}
