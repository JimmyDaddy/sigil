import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> {
  readonly "aria-label": string;
  readonly icon: ReactNode;
  readonly selected?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton({
  icon,
  selected,
  className = "",
  ...props
}: IconButtonProps, ref) {
  return (
    <button
      className={`sg-icon-button ${className}`.trim()}
      ref={ref}
      aria-pressed={selected}
      {...props}
    >
      {icon}
    </button>
  );
});
