import type { ButtonHTMLAttributes, ReactNode } from "react";

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> {
  readonly "aria-label": string;
  readonly icon: ReactNode;
  readonly selected?: boolean;
}

export function IconButton({
  icon,
  selected,
  className = "",
  ...props
}: IconButtonProps) {
  return (
    <button
      className={`sg-icon-button ${className}`.trim()}
      aria-pressed={selected}
      {...props}
    >
      {icon}
    </button>
  );
}
