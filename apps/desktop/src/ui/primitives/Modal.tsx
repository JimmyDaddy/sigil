import {
  useContext,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { useLocale } from "../../i18n";
import { Icon } from "../icons";
import { focusableElements, focusInitial } from "./focus";
import { canRestoreOverlayFocus, isActiveModalOwner, ModalOverlayContext, registerModalLayer } from "./modalIsolation";

interface ModalProps {
  readonly id?: string;
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
  id,
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
  const { t } = useLocale();
  const parentOverlay = useContext(ModalOverlayContext);
  const [overlayRoot, setOverlayRoot] = useState<HTMLDivElement | null>(null);
  const surfaceRef = useRef<HTMLElement>(null);
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  const titleId = `${kind}-${useStableId()}`;
  const descriptionId = `${titleId}-description`;

  useLayoutEffect(() => {
    if (!open || overlayRoot === null) return;
    const surface = surfaceRef.current;
    if (surface === null) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const unregister = registerModalLayer(overlayRoot, parentOverlay);
    const focusTarget = initialFocusRef?.current;
    if (isActiveModalOwner(overlayRoot)) {
      if (focusTarget !== undefined && focusTarget !== null) focusTarget.focus();
      else focusInitial(surface);
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (!isActiveModalOwner(overlayRoot) || event.defaultPrevented) return;
      if (event.key === "Escape") {
        event.preventDefault();
        onOpenChangeRef.current(false);
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = focusableElements(overlayRoot);
      if (focusable.length === 0) {
        event.preventDefault();
        surface.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1] ?? first;
      if (event.shiftKey && (document.activeElement === first || !overlayRoot.contains(document.activeElement))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (document.activeElement === last || !overlayRoot.contains(document.activeElement))) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      unregister();
      const target = returnFocusRef?.current ?? previous;
      if (canRestoreOverlayFocus(target)) target.focus();
    };
  }, [initialFocusRef, open, overlayRoot, parentOverlay, returnFocusRef]);

  if (!open) return null;
  return createPortal(
    <div className={`sg-modal-backdrop sg-modal-${kind}`} data-sigil-overlay-root ref={setOverlayRoot}>
      <ModalOverlayContext.Provider value={overlayRoot}>
        <div
          className="sg-modal-scrim"
          role="presentation"
          onPointerDown={destructive ? undefined : () => onOpenChange(false)}
        />
        <section
          id={id}
          className={`sg-modal-surface sg-modal-surface-${kind}${kind === "drawer" ? ` sg-modal-side-${side}` : ""}`}
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
              <button className="sg-icon-button" type="button" aria-label={t("closeDialog", { title })} onClick={() => onOpenChange(false)}><Icon name="close" /></button>
            )}
          </header>
          <div className="sg-modal-content">{children}</div>
        </section>
      </ModalOverlayContext.Provider>
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
