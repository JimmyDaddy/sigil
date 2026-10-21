import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Dialog, Drawer, Menu, MenuItem, Popover } from ".";

afterEach(cleanup);

function FilterDrawer() {
  const [open, setOpen] = useState(false);
  const [provider, setProvider] = useState("");
  const [source, setSource] = useState("all");
  const [pinned, setPinned] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  return <>
    <button ref={trigger} type="button" onClick={() => setOpen(true)}>Browse conversations</button>
    <button type="button">Background action</button>
    <Drawer open={open} title="Conversations" onOpenChange={setOpen} returnFocusRef={trigger}>
      <Popover label="Filters">
        <label>Provider<input value={provider} onChange={(event) => setProvider(event.target.value)} /></label>
        <label>Source<select value={source} onChange={(event) => setSource(event.target.value)}>
          <option value="all">All</option><option value="ready">Ready</option>
        </select></label>
        <label><input type="checkbox" checked={pinned} onChange={(event) => setPinned(event.target.checked)} />Pinned only</label>
        <button type="button" onClick={() => { setProvider(""); setSource("all"); setPinned(false); }}>Clear filters</button>
      </Popover>
      <output>{`${provider || "any"} / ${source} / ${pinned ? "pinned" : "all"}`}</output>
      <button type="button" onClick={() => setOpen(false)}>Done browsing</button>
    </Drawer>
  </>;
}

describe("modal-owned popovers", () => {
  it("places drawer filters above its surface within the same isolated overlay owner", async () => {
    const user = userEvent.setup();
    const { container } = render(<FilterDrawer />);
    await user.click(screen.getByRole("button", { name: "Browse conversations" }));
    const drawer = screen.getByRole("dialog", { name: "Conversations" });
    await user.click(within(drawer).getByRole("button", { name: "Filters" }));

    const filters = screen.getByRole("dialog", { name: "Filters" });
    const owner = drawer.closest("[data-sigil-overlay-root]");
    expect(filters.parentElement).toBe(owner);
    expect(drawer.contains(filters)).toBe(false);
    expect(owner?.lastElementChild).toBe(filters);
    expect(drawer.getAttribute("aria-owns")).toBe(filters.id);
    expect(filters.closest("[inert], [aria-hidden='true']")).toBeNull();
    expect(container.inert).toBe(true);
    expect(container.getAttribute("aria-hidden")).toBe("true");

    await user.type(screen.getByLabelText("Provider"), "example");
    await user.selectOptions(screen.getByLabelText("Source"), "ready");
    await user.click(screen.getByLabelText("Pinned only"));
    expect(screen.getByRole("status").textContent).toBe("example / ready / pinned");
    await user.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.getByRole("status").textContent).toBe("any / all / all");
  });

  it("focuses filter fields and keeps both Tab directions inside the modal owner", async () => {
    const user = userEvent.setup();
    render(<FilterDrawer />);
    await user.click(screen.getByRole("button", { name: "Browse conversations" }));
    await user.click(screen.getByRole("button", { name: "Filters" }));
    expect(document.activeElement).toBe(screen.getByLabelText("Provider"));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByLabelText("Source"));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByLabelText("Pinned only"));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Clear filters" }));
    await user.tab();
    const close = screen.getByRole("button", { name: "Close Conversations" });
    expect(document.activeElement).toBe(close);
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Clear filters" }));
  });

  it("closes only the popover on first Escape and restores each trigger in order", async () => {
    const user = userEvent.setup();
    const { container } = render(<FilterDrawer />);
    const browse = screen.getByRole("button", { name: "Browse conversations" });
    await user.click(browse);
    const filters = screen.getByRole("button", { name: "Filters" });
    await user.click(filters);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Filters" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Conversations" })).toBeTruthy();
    expect(document.activeElement).toBe(filters);
    expect(container.inert).toBe(true);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(browse);
    expect(container.getAttribute("aria-hidden")).toBeNull();
    expect(container.inert).not.toBe(true);
  });

  it("lets a nested confirmation own Escape before its still-open parent popover", async () => {
    const user = userEvent.setup();
    function Fixture() {
      const [drawer, setDrawer] = useState(true);
      const [confirmation, setConfirmation] = useState(false);
      return <Drawer open={drawer} title="Navigation" onOpenChange={setDrawer}>
        <Popover label="Actions">
          <button type="button" onClick={() => setConfirmation(true)}>Review action</button>
          <Dialog open={confirmation} title="Confirm action" onOpenChange={setConfirmation}>
            <button type="button">Confirm</button>
          </Dialog>
        </Popover>
      </Drawer>;
    }
    render(<Fixture />);
    await user.click(screen.getByRole("button", { name: "Actions" }));
    const review = screen.getByRole("button", { name: "Review action" });
    await user.click(review);
    expect(screen.getByRole("dialog", { name: "Confirm action" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Actions" })).toBeNull();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Confirm action" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Actions" })).toBeTruthy();
    expect(document.activeElement).toBe(review);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Actions" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Navigation" })).toBeTruthy();
  });

  it("dismisses outside without orphaning a panel when its drawer closes", async () => {
    const user = userEvent.setup();
    render(<FilterDrawer />);
    const browse = screen.getByRole("button", { name: "Browse conversations" });
    await user.click(browse);
    await user.click(screen.getByRole("button", { name: "Filters" }));
    await user.click(screen.getByRole("button", { name: "Done browsing" }));
    expect(document.querySelector(".sg-popover-panel")).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(browse);
  });

  it("keeps nested popovers ordered and restores their triggers without dismissing the drawer", async () => {
    const user = userEvent.setup();
    render(<Drawer open title="Navigation" onOpenChange={() => {}}>
      <Popover label="Filters"><input aria-label="Provider" />
        <Popover label="More filters"><button type="button">Pinned</button></Popover>
      </Popover>
    </Drawer>);
    await user.click(screen.getByRole("button", { name: "Filters" }));
    const more = screen.getByRole("button", { name: "More filters" });
    await user.click(more);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Pinned" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "More filters" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Filters" })).toBeTruthy();
    expect(document.activeElement).toBe(more);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Filters" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Navigation" }).getAttribute("aria-owns")).toBeNull();
  });

  it("keeps initially open nested modals in owner order and restores preexisting isolation", async () => {
    const { container, unmount } = render(<Drawer open title="Outer" onOpenChange={() => {}}>
      <Dialog open title="Inner" onOpenChange={() => {}}><button type="button">Inner action</button></Dialog>
    </Drawer>);
    const roots = [...document.querySelectorAll<HTMLElement>("[data-sigil-overlay-root]")];
    const inner = screen.getByRole("dialog", { name: "Inner" });
    const outer = screen.getByRole("dialog", { name: "Outer", hidden: true });
    expect(inner.closest<HTMLElement>("[data-sigil-overlay-root]")?.inert).not.toBe(true);
    expect(outer.closest<HTMLElement>("[data-sigil-overlay-root]")?.inert).toBe(true);
    expect(inner.closest<HTMLElement>("[data-sigil-overlay-root]")?.style.getPropertyValue("--sg-modal-depth")).toBe("1");
    expect(inner.contains(document.activeElement)).toBe(true);
    expect(roots).toHaveLength(2);
    unmount();
    expect(container.inert).not.toBe(true);
    expect(container.getAttribute("aria-hidden")).toBeNull();
  });

  it("isolates newly appended background nodes and restores their exact original state", async () => {
    const user = userEvent.setup();
    render(<FilterDrawer />);
    await user.click(screen.getByRole("button", { name: "Browse conversations" }));
    const background = document.createElement("div");
    background.setAttribute("aria-hidden", "false");
    background.inert = false;
    document.body.append(background);
    try {
      await waitFor(() => expect(background.inert).toBe(true));
      expect(background.getAttribute("aria-hidden")).toBe("true");
      await user.click(screen.getByRole("button", { name: "Filters" }));
      expect(screen.getByRole("dialog", { name: "Filters" }).closest("[inert]")).toBeNull();
      await user.click(screen.getByRole("button", { name: "Done browsing" }));
      expect(background.inert).toBe(false);
      expect(background.getAttribute("aria-hidden")).toBe("false");
    } finally {
      background.remove();
    }
  });

  it("keeps a menu action from stealing focus back after it opens a new modal", async () => {
    const user = userEvent.setup();
    function Fixture() {
      const [confirmation, setConfirmation] = useState(false);
      return <>
        <Menu label="Menu"><MenuItem onSelect={() => setConfirmation(true)}>Review</MenuItem></Menu>
        <Dialog open={confirmation} title="Review" onOpenChange={setConfirmation}><button type="button">Confirm</button></Dialog>
      </>;
    }
    render(<Fixture />);
    await user.click(screen.getByRole("button", { name: "Menu" }));
    await user.click(screen.getByRole("menuitem", { name: "Review" }));
    const dialog = screen.getByRole("dialog", { name: "Review" });
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    expect(dialog.contains(document.activeElement)).toBe(true);
  });

  it("retains the ordinary body portal when no modal owns a popover", async () => {
    const user = userEvent.setup();
    const { container } = render(<Popover label="Workspace"><button type="button">Open workspace</button></Popover>);
    await user.click(screen.getByRole("button", { name: "Workspace" }));
    const panel = screen.getByRole("dialog", { name: "Workspace" });
    expect(panel.parentElement).toBe(document.body);
    expect(container.inert).not.toBe(true);
    fireEvent.pointerDown(container);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
