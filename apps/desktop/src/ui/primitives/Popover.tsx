import {
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { focusInitial } from "./focus";

export interface PopoverProps {
  readonly label: ReactNode;
  readonly accessibleLabel?: string;
  readonly children: ReactNode;
  readonly className?: string;
  readonly open?: boolean;
  readonly onOpenChange?: (open: boolean) => void;
  readonly triggerPopup?: "dialog" | "menu";
  readonly panelRole?: "dialog" | "presentation";
  readonly triggerRef?: RefObject<HTMLButtonElement | null>;
  readonly align?: "start" | "end";
}

export function Popover({
  label,
  accessibleLabel,
  children,
  className = "",
  open: controlledOpen,
  onOpenChange,
  triggerPopup = "dialog",
  panelRole = "dialog",
  triggerRef: externalTriggerRef,
  align = "end",
}: PopoverProps) {
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const rootRef = useRef<HTMLDivElement>(null);
  const localTriggerRef = useRef<HTMLButtonElement>(null);
  const triggerRef = externalTriggerRef ?? localTriggerRef;
  const panelRef = useRef<HTMLDivElement>(null);
  const [panelStyle, setPanelStyle] = useState<CSSProperties>();
  const panelId = useId();
  const panelLabel = accessibleLabel ?? (typeof label === "string" ? label : "Popover");
  const updateOpen = (next: boolean) => {
    if (controlledOpen === undefined) setInternalOpen(next);
    onOpenChange?.(next);
  };

  useEffect(() => {
    if (!open) return;
    const handlePointer = (event: PointerEvent) => {
      if (
        event.target instanceof Node
        && !rootRef.current?.contains(event.target)
        && !panelRef.current?.contains(event.target)
      ) {
        updateOpen(false);
      }
    };
    const handleKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopImmediatePropagation();
      updateOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener("pointerdown", handlePointer);
    document.addEventListener("keydown", handleKey, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointer);
      document.removeEventListener("keydown", handleKey, true);
    };
  });

  useLayoutEffect(() => {
    if (!open) {
      setPanelStyle(undefined);
      return;
    }
    const panel = panelRef.current;
    const trigger = triggerRef.current;
    if (panel === null || trigger === null) return;
    const keepInsideViewport = () => {
      const margin = 12;
      const gap = 8;
      const triggerBounds = trigger.getBoundingClientRect();
      const panelBounds = panel.getBoundingClientRect();
      const availableBelow = Math.max(0, window.innerHeight - margin - triggerBounds.bottom - gap);
      const availableAbove = Math.max(0, triggerBounds.top - margin - gap);
      const placeAbove = panelBounds.height > availableBelow && availableAbove > availableBelow;
      const availableHeight = placeAbove ? availableAbove : availableBelow;
      const renderedHeight = Math.min(panelBounds.height, availableHeight);
      const preferredLeft = align === "start"
        ? triggerBounds.left
        : triggerBounds.right - panelBounds.width;
      const left = Math.min(
        Math.max(margin, preferredLeft),
        Math.max(margin, window.innerWidth - margin - panelBounds.width),
      );
      const top = placeAbove
        ? Math.max(margin, triggerBounds.top - gap - renderedHeight)
        : Math.min(triggerBounds.bottom + gap, window.innerHeight - margin - renderedHeight);
      setPanelStyle({
        position: "fixed",
        top,
        right: "auto",
        bottom: "auto",
        left,
        maxHeight: availableHeight,
        overflowY: "auto",
      });
    };
    keepInsideViewport();
    window.addEventListener("resize", keepInsideViewport);
    window.addEventListener("scroll", keepInsideViewport, true);
    return () => {
      window.removeEventListener("resize", keepInsideViewport);
      window.removeEventListener("scroll", keepInsideViewport, true);
    };
  }, [align, open, triggerRef]);

  const panelClassName = [
    "sg-popover-panel",
    ...className.split(/\s+/).filter(Boolean).map((name) => `${name}-panel`),
  ].join(" ");

  return (
    <div className={`sg-popover ${className}`.trim()} ref={rootRef}>
      <button
        className="sg-popover-trigger"
        ref={triggerRef}
        type="button"
        aria-label={accessibleLabel}
        aria-haspopup={triggerPopup}
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        onClick={() => updateOpen(!open)}
        onKeyDown={(event) => {
          if (event.key !== "ArrowDown" || open) return;
          event.preventDefault();
          updateOpen(true);
          window.requestAnimationFrame(() => panelRef.current && focusInitial(panelRef.current));
        }}
      >
        {label}
      </button>
      {open ? createPortal(
        <div
          className={panelClassName}
          id={panelId}
          ref={panelRef}
          role={panelRole}
          aria-label={panelRole === "dialog" ? panelLabel : undefined}
          tabIndex={-1}
          style={panelStyle}
        >
          {children}
        </div>,
        document.body,
      ) : null}
    </div>
  );
}

export function Menu({ label, accessibleLabel, children }: { readonly label: ReactNode; readonly accessibleLabel?: string; readonly children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const dismiss = () => {
    setOpen(false);
    window.requestAnimationFrame(() => triggerRef.current?.focus());
  };
  return (
    <Popover
      label={label}
      accessibleLabel={accessibleLabel}
      className="sg-menu"
      open={open}
      onOpenChange={setOpen}
      triggerPopup="menu"
      panelRole="presentation"
      triggerRef={triggerRef}
    >
      <MenuDismissContext.Provider value={dismiss}>
        <MenuItems>{children}</MenuItems>
      </MenuDismissContext.Provider>
    </Popover>
  );
}

const MenuDismissContext = createContext<() => void>(() => undefined);

function MenuItems({ children }: { readonly children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>("[role='menuitem']:not([aria-disabled='true'])")?.focus();
  }, []);
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const items = [...(ref.current?.querySelectorAll<HTMLElement>("[role='menuitem']:not([aria-disabled='true'])") ?? [])];
    if (items.length === 0) return;
    const current = Math.max(0, items.indexOf(document.activeElement as HTMLElement));
    let next: number | undefined;
    if (event.key === "ArrowDown") next = (current + 1) % items.length;
    else if (event.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
      const needle = event.key.toLocaleLowerCase();
      next = items.findIndex((item, index) => index > current && item.textContent?.trim().toLocaleLowerCase().startsWith(needle));
      if (next < 0) next = items.findIndex((item) => item.textContent?.trim().toLocaleLowerCase().startsWith(needle));
    }
    if (next === undefined || next < 0) return;
    event.preventDefault();
    items[next]?.focus();
  };
  return <div className="sg-menu-items" ref={ref} role="menu" onKeyDown={onKeyDown}>{children}</div>;
}

export function MenuItem({ children, disabled = false, onSelect }: { readonly children: ReactNode; readonly disabled?: boolean; readonly onSelect?: () => void }) {
  const dismiss = useContext(MenuDismissContext);
  return (
    <button
      className="sg-menu-item"
      type="button"
      role="menuitem"
      tabIndex={-1}
      aria-disabled={disabled || undefined}
      onClick={disabled ? undefined : () => {
        onSelect?.();
        dismiss();
      }}
    >
      {children}
    </button>
  );
}
