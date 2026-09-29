import { lazy, memo, Suspense } from "react";
import type { Props } from "./MarkdownViewImpl";

/**
 * Lazy shell around the markdown renderer.
 *
 * The real component (MarkdownViewImpl) pulls in react-markdown, remark-gfm,
 * rehype-highlight and the highlight.js grammar set — the single heaviest
 * static dependency of the always-mounted ChatPane. Splitting it here lets the
 * shell paint before that chunk is parsed; App warms the chunk right after
 * first paint so it's normally resident before any message needs it.
 *
 * The fallback renders the raw source with the same `.md-view` wrapper and
 * pre-wrapped text so the bubble occupies (approximately) its final height —
 * no empty gap, no layout collapse — for the rare frame the chunk isn't in yet.
 */
const Impl = lazy(() => import("./MarkdownViewImpl"));

/** Kick off loading the renderer chunk without rendering anything. */
export function preloadMarkdownView(): Promise<unknown> {
  return import("./MarkdownViewImpl");
}

export const MarkdownView = memo(function MarkdownView({ source }: Props) {
  return (
    <Suspense
      fallback={
        <div className="md-view md-view-pending">
          <p style={{ whiteSpace: "pre-wrap", margin: 0 }}>{source}</p>
        </div>
      }
    >
      <Impl source={source} />
    </Suspense>
  );
});
