import { useStore } from "../lib/store";

const LABEL: Record<string, string> = {
  open: "live",
  connecting: "…",
  closed: "offline",
};

export default function WsPill() {
  const { wsStatus, serverHealth } = useStore();

  // The health probe is the authoritative reachability source: if the server
  // itself can't be reached, report "offline" regardless of the WS state
  // (which may just be mid-reconnect). Otherwise reflect the live stream.
  const offline = serverHealth === "offline";
  const cls = offline ? "closed" : wsStatus;
  const label = offline ? "offline" : LABEL[wsStatus] ?? wsStatus;
  const title = `Stream: ${wsStatus} · Server: ${serverHealth}`;

  return (
    <span
      className={`ws-pill ${cls}`}
      title={title}
      role="status"
      aria-label={`Connection ${label}`}
    >
      <span className="led" />
      {label}
    </span>
  );
}
