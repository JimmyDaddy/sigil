import type { ReactNode } from "react";

export function Toast({ children, urgent = false }: { readonly children: ReactNode; readonly urgent?: boolean }) {
  return <div className="sg-toast" role={urgent ? "alert" : "status"} aria-live={urgent ? "assertive" : "polite"}>{children}</div>;
}
