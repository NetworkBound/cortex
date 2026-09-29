import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "./http";

/**
 * Load-once-then-refresh helper for list screens: `data` stays visible while a
 * refresh is in flight, `loading` is only true before the first result, and
 * `error` is a message string. `deps` re-run the loader (e.g. resync nonce).
 */
export function useAsync<T>(
  loader: () => Promise<T>,
  deps: unknown[],
  opts: { enabled?: boolean } = {},
) {
  const enabled = opts.enabled ?? true;
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(enabled);
  const [refreshing, setRefreshing] = useState(false);
  const fn = useRef(loader);
  fn.current = loader;
  const gen = useRef(0);

  const run = useCallback(async () => {
    if (!enabled) return;
    const g = ++gen.current;
    setRefreshing(true);
    try {
      const d = await fn.current();
      if (g !== gen.current) return;
      setData(d);
      setError(null);
    } catch (e) {
      if (g !== gen.current) return;
      setError(errorMessage(e));
    } finally {
      if (g === gen.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [enabled]);

  useEffect(() => {
    run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run, ...deps]);

  return { data, setData, error, loading, refreshing, refresh: run };
}
