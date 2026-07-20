import {
  createContext,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";

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
}: PopoverProps) {
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const rootRef = useRef<HTMLDivElement>(null);
  const localTriggerRef = useRef<HTMLButtonElement>(null);
  const triggerRef = externalTriggerRef ?? localTriggerRef;
  const panelRef = useRef<HTMLDivElement>(null);
  const panelId = useId();
  const updateOpen = (next: boolean) => {
    if (controlledOpen === undefined) setInternalOpen(next);
    onOpenChange?.(next);
  };

  useEffect(() => {
    if (!open) return;
    const handlePointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !rootRef.current?.contains(event.target)) updateOpen(false);
    };
    const handleKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      updateOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener("pointerdown", handlePointer);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("pointerdown", handlePointer);
      document.removeEventListener("keydown", handleKey);
    };
  });

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
      {open ? (
        <div className="sg-popover-panel" id={panelId} ref={panelRef} role={panelRole} tabIndex={-1}>
          {children}
        </div>
      ) : null}
    </div>
  );
}

export function Menu({ label, children }: { readonly label: ReactNode; readonly children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <Popover
      label={label}
      className="sg-menu"
      open={open}
      onOpenChange={setOpen}
      triggerPopup="menu"
      panelRole="presentation"
    >
      <MenuDismissContext.Provider value={() => setOpen(false)}>
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
