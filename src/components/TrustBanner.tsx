import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { getTrustStatus, trustProject } from "@/lib/trust";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import "@/styles/trust-banner.css";

/**
 * Top-of-workspace banner that surfaces the backend project-trust state.
 *
 * When the active project is sandboxed read-only (untrusted), this renders a
 * non-blocking bar offering to trust it. While the trust status is loading,
 * when the project is trusted, when there's no active project, or when the
 * user dismisses it for the session, it renders nothing.
 *
 * Fails closed: any backend rejection just toasts and renders nothing — the
 * banner never blocks the UI or crashes the tree. The lead mounts this in
 * App.tsx; it does NOT mount itself.
 */
export function TrustBanner() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const root = activeProject?.root ?? null;

  // `null` = unknown/loading, `true`/`false` = resolved trust state.
  const [trusted, setTrusted] = useState<boolean | null>(null);
  // Roots the user dismissed this session — keep the banner hidden for them
  // even though the backend still reports them as untrusted.
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  // Re-check trust whenever the active project root changes. We track the
  // root the request was issued for and ignore stale responses, so a rapid
  // A → B → A switch can't leave us showing B's verdict for A.
  useEffect(() => {
    if (!root) {
      setTrusted(null);
      return;
    }
    let cancelled = false;
    const requestedFor = root;
    setTrusted(null); // loading — render nothing until resolved
    void getTrustStatus(requestedFor)
      .then((status) => {
        if (cancelled || requestedFor !== root) return; // stale
        setTrusted(status);
      })
      .catch((err) => {
        if (cancelled || requestedFor !== root) return;
        // Fail closed: render nothing, surface the error once.
        setTrusted(null);
        pushToast({
          title: "Couldn't check project trust",
          body: humanizeError(err),
          kind: "error",
        });
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  async function onTrust() {
    if (!root || busy) return;
    const requestedFor = root;
    setBusy(true);
    try {
      await trustProject(requestedFor);
      // Re-check so the banner reflects the authoritative backend state
      // (and disappears) rather than optimistically assuming success.
      const status = await getTrustStatus(requestedFor);
      if (requestedFor !== root) return; // switched projects mid-flight
      setTrusted(status);
      pushToast({
        title: "Project trusted",
        body: "Full tooling is now enabled for this project.",
        kind: "success",
      });
    } catch (err) {
      pushToast({
        title: "Couldn't trust project",
        body: humanizeError(err),
        kind: "error",
      });
    } finally {
      setBusy(false);
    }
  }

  function onDismiss() {
    if (!root) return;
    setDismissed((prev) => {
      const next = new Set(prev);
      next.add(root);
      return next;
    });
  }

  // Render nothing unless we have a resolved, untrusted, non-dismissed root.
  if (!root) return null;
  if (trusted === null) return null; // loading or errored — fail closed
  if (trusted) return null;
  if (dismissed.has(root)) return null;

  return (
    <div className="trust-banner" role="region" aria-label="Project trust">
      <span className="trust-banner-icon" aria-hidden="true">
        🔒
      </span>
      <span className="trust-banner-msg">
        This project is sandboxed read-only until you trust it.
      </span>
      <span className="trust-banner-actions">
        <button
          type="button"
          className="trust-banner-btn trust-banner-btn-primary"
          onClick={() => void onTrust()}
          disabled={busy}
        >
          {busy ? "Trusting…" : "Trust project"}
        </button>
        <button
          type="button"
          className="trust-banner-btn"
          onClick={onDismiss}
          disabled={busy}
        >
          Keep read-only
        </button>
      </span>
    </div>
  );
}
