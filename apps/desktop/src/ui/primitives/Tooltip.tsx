import { cloneElement, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";

export function Tooltip({ label, children }: { readonly label: ReactNode; readonly children: ReactElement<{ "aria-describedby"?: string }> }) {
  const id = useId();
  const anchorRef = useRef<HTMLSpanElement>(null);
  const tooltipRef = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ left: number; top: number; placement: "above" | "below" }>();
  const describedBy = [children.props["aria-describedby"], id].filter(Boolean).join(" ");

  useLayoutEffect(() => {
    if (!open) {
      setPosition(undefined);
      return;
    }
    const updatePosition = () => {
      const anchor = anchorRef.current;
      const tooltip = tooltipRef.current;
      if (anchor === null || tooltip === null) return;
      const margin = 12;
      const gap = 8;
      const anchorBounds = anchor.getBoundingClientRect();
      const tooltipBounds = tooltip.getBoundingClientRect();
      const halfWidth = tooltipBounds.width / 2;
      const desiredLeft = anchorBounds.left + anchorBounds.width / 2;
      const left = Math.min(
        Math.max(desiredLeft, margin + halfWidth),
        window.innerWidth - margin - halfWidth,
      );
      const placement = anchorBounds.top >= tooltipBounds.height + gap + margin ? "above" : "below";
      setPosition({
        left,
        top: placement === "above" ? anchorBounds.top - gap : anchorBounds.bottom + gap,
        placement,
      });
    };
    updatePosition();
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [open, label]);

  const style = position === undefined ? undefined : ({
    left: position.left,
    top: position.top,
    transform: position.placement === "above" ? "translate(-50%, -100%)" : "translateX(-50%)",
  } satisfies CSSProperties);
  return (
    <span
      className="sg-tooltip-anchor"
      ref={anchorRef}
      onPointerEnter={() => setOpen(true)}
      onPointerLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") setOpen(false);
      }}
    >
      {cloneElement(children, { "aria-describedby": describedBy })}
      {open ? createPortal(
        <span
          className="sg-tooltip"
          id={id}
          ref={tooltipRef}
          role="tooltip"
          data-placement={position?.placement}
          style={style}
        >
          {label}
        </span>,
        document.body,
      ) : null}
    </span>
  );
}
