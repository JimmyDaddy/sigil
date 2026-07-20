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
    expect(screen.getByLabelText("State").tagName).toBe("SELECT");
    expect(screen.getByLabelText("Pinned").getAttribute("type")).toBe("checkbox");
  });

  it("provides disclosure, tooltip, toast, and bounded button states", async () => {
    const user = userEvent.setup();
    render(
      <>
        <Button busy>Saving</Button>
        <Collapsible label="Evidence" summary="2 items"><p>Receipt</p></Collapsible>
        <Tooltip label="Nonessential hint"><button type="button">Hover target</button></Tooltip>
        <Toast>Saved locally</Toast>
      </>,
    );
    expect((screen.getByRole("button", { name: "Saving" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("tooltip").textContent).toBe("Nonessential hint");
    expect(screen.getByRole("status").textContent).toBe("Saved locally");
    await user.click(screen.getByText("Evidence"));
    expect((screen.getByText("Evidence").closest("details") as HTMLDetailsElement).open).toBe(true);
  });

  it("dismisses popovers outside or with Escape and restores trigger focus", async () => {
    const user = userEvent.setup();
    render(<><Popover label="Filters"><button type="button">Apply filters</button></Popover><button type="button">Outside</button></>);
    const trigger = screen.getByRole("button", { name: "Filters" });
    await user.click(trigger);
    expect(screen.getByRole("dialog")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);

    await user.click(trigger);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Outside" }));
    expect(screen.queryByRole("dialog")).toBeNull();
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
    expect(screen.getAllByRole("dialog")).toHaveLength(2);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Open drawer" }));
    expect(screen.getByRole("dialog", { name: "Navigation" })).toBeTruthy();
  });
});
