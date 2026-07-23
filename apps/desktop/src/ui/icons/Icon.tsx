import type { SVGAttributes } from "react";

export type IconName = "add" | "agents" | "appearance-auto" | "back" | "check" | "chevron-down" | "chevron-up" | "close" | "copy" | "delete" | "download" | "edit" | "extensions" | "external" | "filter" | "history" | "interrupt-next" | "language" | "library" | "lock" | "menu" | "model" | "moon" | "more" | "pause" | "pin" | "play" | "queue" | "search" | "send" | "settings" | "shield" | "stop" | "sun" | "warning";

const paths: Record<IconName, string> = {
  add: "M12 5v14M5 12h14",
  agents: "M8 7a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm8 4a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5ZM3 20v-2a5 5 0 0 1 10 0v2m1-5a4 4 0 0 1 7 2.7V20",
  "appearance-auto": "M12 3a9 9 0 1 0 0 18V3Z",
  back: "m15 18-6-6 6-6M9 12h11",
  check: "m5 12 4 4L19 6",
  "chevron-down": "m6 9 6 6 6-6",
  "chevron-up": "m6 15 6-6 6 6",
  close: "m6 6 12 12M18 6 6 18",
  copy: "M9 8h10v11H9zM5 15V5h10",
  delete: "M4 7h16m-10 4v6m4-6v6M8 7l1-3h6l1 3m-9 0 1 13h8l1-13",
  download: "M12 3v12m-5-5 5 5 5-5M5 20h14",
  edit: "m4 20 4.5-1 10-10a2.1 2.1 0 0 0-3-3l-10 10L4 20Zm10-12 3 3",
  extensions: "M8 4h8v5h4v7h-4v4H8v-4H4V9h4V4Zm3 5h2m-2 6h2",
  external: "M14 4h6v6m0-6-9 9M19 14v5H5V5h5",
  filter: "M4 6h16M7 12h10m-7 6h4",
  history: "M4 12a8 8 0 1 0 2.3-5.7L4 8m0-5v5h5m3-1v5l3 2",
  "interrupt-next": "M4 7h9m-9 5h6m-6 5h9m3-11v12m0-6 4 4m-4-4 4-4",
  language: "M4 5h10M9 3v2m-3 4c1.6 3.2 4.4 5.5 8 6.5M13 8c-1.2 4-4.1 7.2-8 9m10-5 4 9m-6 0 4-9 4 9m-6-3h6",
  library: "M4 5h16v14H4zM8 5v14m4-10h5m-5 4h5",
  lock: "M7 11V8a5 5 0 0 1 10 0v3M5 11h14v9H5z",
  menu: "M4 7h16M4 12h16M4 17h16",
  model: "M12 3 4 7v10l8 4 8-4V7l-8-4Zm-8 4 8 4 8-4M12 11v10",
  moon: "M20 15.2A8.5 8.5 0 0 1 8.8 4 8.5 8.5 0 1 0 20 15.2Z",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  pause: "M8 5v14m8-14v14",
  pin: "m15 4 5 5-3 1.5-3.5 3.5 1 3-1.5 1.5-3.5-3.5L5 19l1.5-5.5L3 10l3.5-3.5 3 1Z",
  play: "M7 4v16l13-8L7 4Z",
  queue: "M5 6h11m-11 6h8m-8 6h11m2-9v9m0 0 3-3m-3 3-3-3",
  search: "m20 20-4.3-4.3m2.3-5.2a7.5 7.5 0 1 1-15 0 7.5 7.5 0 0 1 15 0Z",
  send: "m4 4 17 8-17 8 3-8-3-8Zm3 8h14",
  settings: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm0-5 1 2.2 2.4.7 2-1.3 2 2-1.3 2 .7 2.4L21 12l-2.2 1-.7 2.4 1.3 2-2 2-2-1.3-2.4.7L12 21l-1-2.2-2.4-.7-2 1.3-2-2 1.3-2L5 13 3 12l2-1 .7-2.4-1.3-2 2-2 2 1.3L11 5l1-2Z",
  shield: "M12 3 20 6v5c0 5-3.4 8.3-8 10-4.6-1.7-8-5-8-10V6l8-3Z",
  stop: "M7 7h10v10H7z",
  sun: "M12 3v2m0 14v2M3 12h2m14 0h2M5.64 5.64l1.42 1.42m9.88 9.88 1.42 1.42m0-12.72-1.42 1.42M7.06 16.94l-1.42 1.42M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  warning: "M12 4 21 20H3L12 4Zm0 5v5m0 3h.01",
};

export function Icon({ name, ...props }: { readonly name: IconName } & SVGAttributes<SVGSVGElement>) {
  return (
    <svg
      className="sg-icon"
      viewBox="0 0 24 24"
      width="18"
      height="18"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...props}
    >
      {name === "appearance-auto" ? (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d={paths[name]} fill="currentColor" stroke="none" />
        </>
      ) : <path d={paths[name]} />}
    </svg>
  );
}
