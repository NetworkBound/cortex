import { useStore } from "../lib/store";

const LABEL: Record<string, string> = {
  open: "live",
  connecting: "…",
  closed: "offline",
};

export default function WsPill() {
  const { wsStatus } = useStore();
  return (
    <span className={`ws-pill ${wsStatus}`}>
      <span className="led" />
      {LABEL[wsStatus] ?? wsStatus}
    </span>
  );
}
