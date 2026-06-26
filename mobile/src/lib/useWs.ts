import { useEffect, useRef } from "react";
import { bus } from "./ws";
import type { WsFrameBase } from "./types";

/**
 * Subscribe to the shared WS bus. The handler is kept in a ref so callers can
 * pass an inline closure without resubscribing every render.
 */
export function useWs(handler: (frame: WsFrameBase) => void) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    return bus.subscribe((f) => ref.current(f));
  }, []);
}
