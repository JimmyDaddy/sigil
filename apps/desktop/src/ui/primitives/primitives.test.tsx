import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useRef, useState } from "react";

import {
  Button,
  Checkbox,
  Collapsible,
  Dialog,
  Drawer,
  Menu,
  MenuItem,
  Popover,
  Select,
  TextArea,
  TextField,
  Toast,
  Tooltip,
} from ".";

afterEach(cleanup);

describe("Sigil UI primitives", () => {
  it("binds field labels, help, errors, native form values, and IME-safe textarea input", async () => {
    const user = userEvent.setup();
    render(
      <form>
        <TextField label="Search" description="Search saved conversations" />
        <TextArea label="Prompt" error="Prompt is required" />
        <Select label="State" defaultValue="ready"><option value="ready">Ready</option></Select>
        <Checkbox label="Pinned" description="Only pinned conversations" />
      </form>,
    );
    const prompt = screen.getByLabelText("Prompt") as HTMLTextAreaElement;
    fireEvent.compositionStart(prompt);
    await user.type(prompt, "你好");
    fireEvent.compositionEnd(prompt);
    expect(prompt.value).toBe("你好");
    expect(prompt.getAttribute("aria-invalid")).toBe("true");
    const state = screen.getByLabelText("State");
    expect(state.tagName).toBe("SELECT");
    expect(state.parentElement?.classList.contains("sg-select-control")).toBe(true);
    expect(state.parentElement?.querySelector(".sg-select-indicator")).toBeTruthy();
    expect(screen.getByLabelText("Pinned").getAttribute("type")).toBe("checkbox");
  });

  it("provides disclosure, tooltip, toast, and bounded button states", async () => {
    const user = userEvent.setup();
    const dismissToast = vi.fn();
    render(
      <>
        <Button busy>Saving</Button>
        <Collapsible label="Evidence" summary="2 items"><p>Receipt</p></Collapsible>
        <Tooltip label="Nonessential hint"><button type="button">Hover target</button></Tooltip>
        <Toast tone="success" title="Saved" timeoutMs={4_000} onDismiss={dismissToast}>Saved locally</Toast>
      </>,
    );
    expect((screen.getByRole("button", { name: "Saving" }) as HTMLButtonElement).disabled).toBe(true);
    await user.hover(screen.getByRole("button", { name: "Hover target" }));
    expect(screen.getByRole("tooltip").textContent).toBe("Nonessential hint");
    expect(screen.getByRole("status").textContent).toContain("Saved locally");
    expect(screen.getByRole("status").classList.contains("sg-toast-success")).toBe(true);
    await user.click(screen.getByRole("button", { name: "Dismiss notification" }));
    expect(dismissToast).toHaveBeenCalledOnce();
    await user.click(screen.getByText("Evidence"));
    expect((screen.getByText("Evidence").closest("details") as HTMLDetailsElement).open).toBe(true);
  });

  it("dismisses popovers outside or with Escape and restores trigger focus", async () => {
    const user = userEvent.setup();
    render(<><Popover label="Filters"><button type="button">Apply filters</button></Popover><button type="button">Outside</button></>);
    const trigger = screen.getByRole("button", { name: "Filters" });
    await user.click(trigger);
    expect(screen.getByRole("dialog", { name: "Filters" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);

    await user.click(trigger);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Outside" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("moves an edge popover back inside the visible viewport", async () => {
    const bounds = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      return {
        x: -52,
        y: 40,
        width: 240,
        height: 180,
        top: 40,
        right: 188,
        bottom: 220,
        left: -52,
        toJSON: () => ({}),
      };
    });
    const user = userEvent.setup();
    render(<Popover label="Edge filters"><span>Visible controls</span></Popover>);

    await user.click(screen.getByRole("button", { name: "Edge filters" }));

    await waitFor(() => {
      expect(screen.getByRole("dialog", { name: "Edge filters" }).style.left).toBe("12px");
    });
    bounds.mockRestore();
  });

  it("portals and flips a bottom-edge popover above its trigger", async () => {
    const bounds = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains("sg-popover-panel")) {
        return { x: 0, y: 0, width: 180, height: 120, top: 0, right: 180, bottom: 120, left: 0, toJSON: () => ({}) };
      }
      return { x: 400, y: 700, width: 40, height: 32, top: 700, right: 440, bottom: 732, left: 400, toJSON: () => ({}) };
    });
    const user = userEvent.setup();
    const { container } = render(<Popover label="Bottom actions"><span>Visible actions</span></Popover>);

    await user.click(screen.getByRole("button", { name: "Bottom actions" }));

    const panel = await screen.findByRole("dialog", { name: "Bottom actions" });
    expect(container.contains(panel)).toBe(false);
    await waitFor(() => expect(panel.style.top).toBe("572px"));
    bounds.mockRestore();
  });

  it("gives a nested popover first ownership of Escape", async () => {
    const user = userEvent.setup();
    function Fixture() {
      const [drawer, setDrawer] = useState(true);
      return (
        <Drawer open={drawer} title="Navigation" onOpenChange={setDrawer}>
          <Popover label="Filters"><button type="button">Apply filters</button></Popover>
        </Drawer>
      );
    }
    render(<Fixture />);
    await user.click(screen.getByRole("button", { name: "Filters" }));

    fireEvent.keyDown(document, { key: "Escape" });

    expect(screen.queryByRole("dialog", { name: "Filters" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Navigation" })).toBeTruthy();
  });

  it("implements menu arrow navigation, disabled skipping, and typeahead", async () => {
    const selected = vi.fn();
    render(
      <Menu label="Actions">
        <MenuItem>Open</MenuItem>
        <MenuItem disabled>Unavailable</MenuItem>
        <MenuItem onSelect={selected}>Save</MenuItem>
      </Menu>,
    );
    const trigger = screen.getByRole("button", { name: "Actions" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    await waitFor(() => expect(document.activeElement?.textContent).toBe("Open"));
    fireEvent.keyDown(screen.getByRole("menu"), { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("Save");
    fireEvent.click(document.activeElement as HTMLElement);
    expect(selected).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(trigger));

    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    await waitFor(() => expect(document.activeElement?.textContent).toBe("Open"));
    fireEvent.keyDown(screen.getByRole("menu"), { key: "o" });
    expect(document.activeElement?.textContent).toBe("Open");
  });

  it("traps modal focus, dismisses with Escape, and restores the trigger", async () => {
    const user = userEvent.setup();
    function Fixture() {
      const [open, setOpen] = useState(false);
      const trigger = useRef<HTMLButtonElement>(null);
      return <><button ref={trigger} type="button" onClick={() => setOpen(true)}>Open dialog</button><Dialog open={open} title="Confirm" onOpenChange={setOpen} returnFocusRef={trigger}><button type="button">First</button><button type="button">Last</button></Dialog></>;
    }
    render(<Fixture />);
    const trigger = screen.getByRole("button", { name: "Open dialog" });
    await user.click(trigger);
    expect(screen.getByRole("dialog", { name: "Confirm" }).classList.contains("sg-modal-side-end")).toBe(false);
    const close = screen.getByRole("button", { name: "Close Confirm" });
    expect(document.activeElement).toBe(close);
    const last = screen.getByRole("button", { name: "Last" });
    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(close);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("keeps nested overlay ownership on the topmost dialog and supports drawers", async () => {
    const user = userEvent.setup();
    function Fixture() {
      const [outer, setOuter] = useState(false);
      const [inner, setInner] = useState(false);
      const [drawer, setDrawer] = useState(false);
      return <>
        <button type="button" onClick={() => setOuter(true)}>Open outer</button>
        <button type="button" onClick={() => setDrawer(true)}>Open drawer</button>
        <Dialog open={outer} title="Outer" onOpenChange={setOuter}>
          <button type="button" onClick={() => setInner(true)}>Open inner</button>
          <Dialog open={inner} title="Inner" onOpenChange={setInner}><button type="button">Inner action</button></Dialog>
        </Dialog>
        <Drawer open={drawer} title="Navigation" onOpenChange={setDrawer}><button type="button">Session</button></Drawer>
      </>;
    }
    render(<Fixture />);
    await user.click(screen.getByRole("button", { name: "Open outer" }));
    await user.click(screen.getByRole("button", { name: "Open inner" }));
    expect(screen.getAllByRole("dialog", { hidden: true })).toHaveLength(2);
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(screen.getByRole("dialog", { name: "Inner" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(screen.getByRole("dialog", { name: "Outer" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Open drawer" }));
    expect(screen.getByRole("dialog", { name: "Navigation" }).classList.contains("sg-modal-side-end")).toBe(true);
  });
});
