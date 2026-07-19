import { useEffect, type RefObject } from "react";

const FOCUSABLE = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function useFocusBoundary({
  active,
  containerRef,
  returnFocusRef,
  onDismiss,
}: {
  active: boolean;
  containerRef: RefObject<HTMLElement | null>;
  returnFocusRef?: RefObject<HTMLElement | null>;
  onDismiss: () => void;
}) {
  useEffect(() => {
    if (!active) return;
    const previous = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : undefined;
    const container = containerRef.current;
    const initial = container?.querySelector<HTMLElement>("[data-initial-focus]")
      ?? container?.querySelector<HTMLElement>(FOCUSABLE)
      ?? container;
    initial?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onDismiss();
        return;
      }
      if (event.key !== "Tab" || container === null) return;
      const focusable = [...container.querySelectorAll<HTMLElement>(FOCUSABLE)]
        .filter((element) => !element.hidden && element.getAttribute("aria-hidden") !== "true");
      if (focusable.length === 0) {
        event.preventDefault();
        container.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable.at(-1) ?? first;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      const target = returnFocusRef?.current ?? previous;
      if (target !== undefined && document.contains(target)) target.focus();
    };
  }, [active, containerRef, onDismiss, returnFocusRef]);
}
