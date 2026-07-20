import type { SVGAttributes } from "react";

export type IconName = "add" | "close" | "filter" | "menu" | "moon" | "more" | "search" | "sun" | "system";

const paths: Record<IconName, string> = {
  add: "M12 5v14M5 12h14",
  close: "m6 6 12 12M18 6 6 18",
  filter: "M4 6h16M7 12h10m-7 6h4",
  menu: "M4 7h16M4 12h16M4 17h16",
  moon: "M20 15.2A8.5 8.5 0 0 1 8.8 4 8.5 8.5 0 1 0 20 15.2Z",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  search: "m20 20-4.3-4.3m2.3-5.2a7.5 7.5 0 1 1-15 0 7.5 7.5 0 0 1 15 0Z",
  sun: "M12 3v2m0 14v2M3 12h2m14 0h2M5.64 5.64l1.42 1.42m9.88 9.88 1.42 1.42m0-12.72-1.42 1.42M7.06 16.94l-1.42 1.42M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  system: "M4 5.5h16v11H4zM9 20h6m-3-3.5V20",
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
      <path d={paths[name]} />
    </svg>
  );
}
