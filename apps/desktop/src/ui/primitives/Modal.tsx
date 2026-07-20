import {
  useEffect,
  useRef,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { Icon } from "../icons";
import { focusableElements, focusInitial } from "./focus";

const modalStack: HTMLElement[] = [];

interface ModalProps {
  readonly open: boolean;
  readonly title: string;
  readonly description?: string;
  readonly onOpenChange: (open: boolean) => void;
  readonly children: ReactNode;
  readonly initialFocusRef?: RefObject<HTMLElement | null>;
  readonly returnFocusRef?: RefObject<HTMLElement | null>;
  readonly kind?: "dialog" | "drawer";
  readonly side?: "start" | "end";
  readonly destructive?: boolean;
}

export function Modal({
  open,
  title,
  description,
  onOpenChange,
  children,
  initialFocusRef,
  returnFocusRef,
  kind = "dialog",
  side = "end",
  destructive = false,
}: ModalProps) {
  const surfaceRef = useRef<HTMLElement>(null);
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  const titleId = `${kind}-${useStableId()}`;
  const descriptionId = `${titleId}-description`;

  useEffect(() => {
    if (!open) return;
    const surface = surfaceRef.current;
    if (surface === null) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const overlayRoot = surface.closest<HTMLElement>("[data-sigil-overlay-root]");
    const inerted = [...document.body.children]
      .filter((element): element is HTMLElement => element instanceof HTMLElement)
      .filter((element) => !element.hasAttribute("data-sigil-overlay-root"))
      .map((element) => ({
        element,
        inert: element.inert,
        ariaHidden: element.getAttribute("aria-hidden"),
      }));
    for (const { element } of inerted) {
      element.inert = true;
      element.setAttribute("aria-hidden", "true");
    }

    modalStack.push(surface);
    const focusTarget = initialFocusRef?.current;
    if (focusTarget !== undefined && focusTarget !== null) focusTarget.focus();
    else focusInitial(surface);

    const handleKeyDown = (event: KeyboardEvent) => {
      if (modalStack.at(-1) !== surface) return;
      if (event.key === "Escape") {
        event.preventDefault();
        onOpenChangeRef.current(false);
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = focusableElements(surface);
      if (focusable.length === 0) {
        event.preventDefault();
        surface.focus();
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
      const index = modalStack.lastIndexOf(surface);
      if (index >= 0) modalStack.splice(index, 1);
      if (modalStack.length === 0) {
        for (const { element, inert, ariaHidden } of inerted) {
          element.inert = inert;
          if (ariaHidden === null) element.removeAttribute("aria-hidden");
          else element.setAttribute("aria-hidden", ariaHidden);
        }
      }
      const target = returnFocusRef?.current ?? previous;
      if (target !== undefined && document.contains(target)) target.focus();
      overlayRoot?.removeAttribute("data-closing");
    };
  }, [initialFocusRef, open, returnFocusRef]);

  if (!open) return null;
  return createPortal(
    <div className={`sg-modal-backdrop sg-modal-${kind}`} data-sigil-overlay-root>
      <div
        className="sg-modal-scrim"
        role="presentation"
        onPointerDown={destructive ? undefined : () => onOpenChange(false)}
      />
      <section
        className={`sg-modal-surface sg-modal-surface-${kind} sg-modal-side-${side}`}
        ref={surfaceRef}
        tabIndex={-1}
        role={destructive ? "alertdialog" : "dialog"}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description === undefined ? undefined : descriptionId}
      >
        <header className="sg-modal-header">
          <div>
            <h2 id={titleId}>{title}</h2>
            {description === undefined ? null : <p id={descriptionId}>{description}</p>}
          </div>
          {destructive ? null : (
            <button className="sg-icon-button" type="button" aria-label={`Close ${title}`} onClick={() => onOpenChange(false)}><Icon name="close" /></button>
          )}
        </header>
        <div className="sg-modal-content">{children}</div>
      </section>
    </div>,
    document.body,
  );
}

let nextId = 0;
function useStableId(): string {
  const id = useRef<string | undefined>(undefined);
  if (id.current === undefined) {
    nextId += 1;
    id.current = String(nextId);
  }
  return id.current;
}

export function Dialog(props: Omit<ModalProps, "kind" | "side">) {
  return <Modal {...props} kind="dialog" />;
}

export function Drawer(props: Omit<ModalProps, "kind">) {
  return <Modal {...props} kind="drawer" />;
}
