import { useEffect, useRef } from "react";
import { bus } from "./ws";
import type { StreamEvent } from "./types";

/** Subscribe to the shared stream. The handler lives in a ref so callers can
 *  pass an inline closure without resubscribing every render. */
export function useWs(handler: (ev: StreamEvent) => void) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => bus.subscribe((ev) => ref.current(ev)), []);
}
