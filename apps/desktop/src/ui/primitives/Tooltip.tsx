import { cloneElement, useId, type ReactElement, type ReactNode } from "react";

export function Tooltip({ label, children }: { readonly label: ReactNode; readonly children: ReactElement<{ "aria-describedby"?: string }> }) {
  const id = useId();
  const describedBy = [children.props["aria-describedby"], id].filter(Boolean).join(" ");
  return (
    <span className="sg-tooltip-anchor">
      {cloneElement(children, { "aria-describedby": describedBy })}
      <span className="sg-tooltip" id={id} role="tooltip">{label}</span>
    </span>
  );
}
