import { pct100 } from "../lib/format";

/** Ring gauge for quota windows. */
export function RingGauge({
  value,
  label,
  sub,
}: {
  value: number | null | undefined;
  label: string;
  sub?: string;
}) {
  const p = pct100(value);
  const r = 26;
  const c = 2 * Math.PI * r;
  const tone = p >= 90 ? "err" : p >= 75 ? "warn" : "ok";
  return (
    <div
      className={`gauge ${tone}`}
      role="img"
      aria-label={`${label} ${Math.round(p)}%`}
    >
      <svg width="68" height="68" viewBox="0 0 68 68">
        <circle className="track" cx="34" cy="34" r={r} />
        <circle
          className="fill"
          cx="34"
          cy="34"
          r={r}
          strokeDasharray={c}
          strokeDashoffset={c * (1 - p / 100)}
          transform="rotate(-90 34 34)"
        />
        <text x="34" y="38" textAnchor="middle" className="gauge-val">
          {value === null || value === undefined ? "—" : `${Math.round(p)}%`}
        </text>
      </svg>
      <div className="gauge-label">{label}</div>
      {sub && <div className="gauge-sub">{sub}</div>}
    </div>
  );
}

export function Bar({
  value,
  max,
  label,
}: {
  value: number;
  max?: number | null;
  label: string;
}) {
  const p = max ? Math.min(100, (value / max) * 100) : 0;
  const tone = p >= 90 ? "err" : p >= 75 ? "warn" : "ok";
  return (
    <div className={`bar ${tone}`}>
      <div className="bar-label">{label}</div>
      <div className="bar-track">
        <div className="bar-fill" style={{ width: `${max ? p : 0}%` }} />
      </div>
    </div>
  );
}
