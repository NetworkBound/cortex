// Inline stroke icons (no icon-font dep). 24px viewBox, currentColor.

const PATHS: Record<string, string> = {
  chat: "M21 12a8 8 0 0 1-8 8H8l-5 3 1.5-4.5A8 8 0 1 1 21 12z",
  inbox: "M3 13h5l2 3h4l2-3h5M5 5h14l2 8v6H3v-6z",
  folder:
    "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  activity: "M3 12h4l3-8 4 16 3-8h4",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  plus: "M12 5v14M5 12h14",
  back: "M15 5l-7 7 7 7",
  chevron: "M9 5l7 7-7 7",
  send: "M12 19V5M5 12l7-7 7 7",
  stop: "M7 7h10v10H7z",
  attach:
    "M21 11.5l-8.5 8.5a5 5 0 0 1-7-7l9-9a3.5 3.5 0 0 1 5 5l-9 9a2 2 0 0 1-3-3l8-8",
  check: "M5 13l4 4L19 7",
  x: "M6 6l12 12M18 6L6 18",
  refresh: "M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5",
  qr: "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h2v2h-2zM18 14h2v2h-2zM14 18h2v2h-2zM18 18h2v2h-2z",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  edit: "M4 20h4l10-10-4-4L4 16zM13 7l4 4",
  play: "M7 5l12 7-12 7z",
  warning: "M12 3l10 18H2zM12 10v4M12 18h.01",
  arrowDown: "M12 5v14M5 12l7 7 7-7",
  git: "M6 3v12M6 15a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM18 9c0 4-4 5-8 6",
  clock: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 7v5l3 2",
  bell: "M6 16V11a6 6 0 1 1 12 0v5l2 2H4zM10 21h4",
  phone:
    "M8 2h8a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2zM11 18h2",
  info: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 11v5M12 8h.01",
  logout: "M10 17l5-5-5-5M15 12H3M14 3h5a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-5",
  sun: "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10zM12 2v2M12 20v2M4 12H2M22 12h-2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5",
  spark: "M12 3l2 6 6 2-6 2-2 6-2-6-6-2 6-2z",
  camera: "M4 8h3l2-3h6l2 3h3v11H4zM12 17a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z",
  upload: "M12 16V4M6 10l6-6 6 6M4 20h16",
  copy: "M8 8h12v12H8zM4 16V4h12",
  dot: "M12 12h.01",
  bolt: "M13 2L4 14h7l-1 8 9-12h-7z",
};

export default function Icon({
  name,
  size = 22,
  className,
}: {
  name: keyof typeof PATHS | string;
  size?: number;
  className?: string;
}) {
  const d = PATHS[name] ?? PATHS.dot;
  return (
    <svg
      className={className ? `ico ${className}` : "ico"}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={name === "dot" || name === "more" ? 3 : 1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={d} />
    </svg>
  );
}
