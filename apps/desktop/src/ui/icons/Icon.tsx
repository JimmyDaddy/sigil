import type { SVGAttributes } from "react";

export type IconName = "add" | "appearance-auto" | "check" | "close" | "copy" | "extensions" | "filter" | "language" | "lock" | "menu" | "model" | "moon" | "more" | "pin" | "search" | "send" | "shield" | "stop" | "sun" | "warning";

const paths: Record<IconName, string> = {
  add: "M12 5v14M5 12h14",
  "appearance-auto": "M12 3a9 9 0 1 0 0 18V3Z",
  check: "m5 12 4 4L19 6",
  close: "m6 6 12 12M18 6 6 18",
  copy: "M9 8h10v11H9zM5 15V5h10",
  extensions: "M8 4h8v5h4v7h-4v4H8v-4H4V9h4V4Zm3 5h2m-2 6h2",
  filter: "M4 6h16M7 12h10m-7 6h4",
  language: "M4 5h10M9 3v2m-3 4c1.6 3.2 4.4 5.5 8 6.5M13 8c-1.2 4-4.1 7.2-8 9m10-5 4 9m-6 0 4-9 4 9m-6-3h6",
  lock: "M7 11V8a5 5 0 0 1 10 0v3M5 11h14v9H5z",
  menu: "M4 7h16M4 12h16M4 17h16",
  model: "M12 3 4 7v10l8 4 8-4V7l-8-4Zm-8 4 8 4 8-4M12 11v10",
  moon: "M20 15.2A8.5 8.5 0 0 1 8.8 4 8.5 8.5 0 1 0 20 15.2Z",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  pin: "m15 4 5 5-3 1.5-3.5 3.5 1 3-1.5 1.5-3.5-3.5L5 19l1.5-5.5L3 10l3.5-3.5 3 1Z",
  search: "m20 20-4.3-4.3m2.3-5.2a7.5 7.5 0 1 1-15 0 7.5 7.5 0 0 1 15 0Z",
  send: "m4 4 17 8-17 8 3-8-3-8Zm3 8h14",
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
