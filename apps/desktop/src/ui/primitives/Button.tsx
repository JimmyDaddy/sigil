import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "quiet" | "danger";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  readonly variant?: ButtonVariant;
  readonly busy?: boolean;
  readonly leadingIcon?: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button({
  variant = "secondary",
  busy = false,
  leadingIcon,
  className = "",
  disabled,
  children,
  ...props
}: ButtonProps, ref) {
  return (
    <button
      className={`sg-button sg-button-${variant} ${className}`.trim()}
      ref={ref}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      {...props}
    >
      {leadingIcon}
      <span>{children}</span>
    </button>
  );
});
