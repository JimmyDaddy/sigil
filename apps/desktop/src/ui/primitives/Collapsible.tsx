import type { DetailsHTMLAttributes, ReactNode } from "react";

export interface CollapsibleProps extends DetailsHTMLAttributes<HTMLDetailsElement> {
  readonly label: ReactNode;
  readonly summary?: ReactNode;
}

export function Collapsible({ label, summary, children, className = "", ...props }: CollapsibleProps) {
  return (
    <details className={`sg-collapsible ${className}`.trim()} {...props}>
      <summary><span>{label}</span>{summary === undefined ? null : <small>{summary}</small>}</summary>
      <div className="sg-collapsible-content">{children}</div>
    </details>
  );
}
