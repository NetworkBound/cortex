import { useCallback, useEffect, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { RefreshCw, FolderOpen } from "lucide-react";
import { useCortexStore } from "@/state/store";
import { repoMapText } from "@/lib/repo-map";
import {
  repoWatcherReset,
  subscribeRepoWatcher,
  type RepoWatcherEvent,
} from "@/lib/repo-watcher";

/**
 * Small StatusBar pill that flags a stale repo map. Subscribes to
 * `repo-watcher:event` events from the backend, accumulates a per-project
 * change count, and offers a one-click re-index that re-runs `repo_map`
 * for the active project.
 *
 * Hidden when there is no active project OR when no changes have landed
 * since the last successful re-index.
 */
export function RepoWatchBadge() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const activeRoot = activeProject?.root ?? null;

  const [changes, setChanges] = useState<number>(0);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Track the root we're counting for so an event from a stale watcher
  // doesn't bleed into a freshly-switched project.
  const rootRef = useRef<string | null>(null);

  useEffect(() => {
    rootRef.current = activeRoot;
    // Reset the local counter on every project switch.
    setChanges(0);
    setErr(null);
  }, [activeRoot]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    void (async () => {
      try {
        const fn = await subscribeRepoWatcher((evt: RepoWatcherEvent) => {
          if (cancelled) return;
          // Only count events that belong to the currently-active project.
          if (rootRef.current && evt.project_root === rootRef.current) {
            setChanges((c) => c + 1);
          }
        });
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      } catch {
        // Subscription failures are non-fatal — the badge simply stays
        // hidden until the next reload.
      }
    })();

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  const reindex = useCallback(async () => {
    if (!activeRoot || busy) return;
    setBusy(true);
    setErr(null);
    try {
      await repoMapText(activeRoot);
      await repoWatcherReset(activeRoot);
      setChanges(0);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [activeRoot, busy]);

  if (!activeProject || changes <= 0) return null;

  const tooltip = `${changes} file${changes === 1 ? "" : "s"} changed since last index — click to re-index`;

  return (
    <button
      type="button"
      className={`status-pill repo-watch-badge${busy ? " busy" : ""}`}
      onClick={() => void reindex()}
      disabled={busy}
      title={err ?? tooltip}
      aria-label="repo stale, re-index"
    >
      <span className="repo-watch-icon" aria-hidden>
        {busy ? (
          <RefreshCw size={13} strokeWidth={1.75} />
        ) : (
          <FolderOpen size={13} strokeWidth={1.75} />
        )}
      </span>
      <span className="repo-watch-text">
        repo: stale ({changes})
      </span>
    </button>
  );
}
