import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LocaleProvider } from "../../i18n";
import type { RecentWorkspaceSummary, WorkspaceSummary } from "../../types";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";

const first: WorkspaceSummary = { id: "workspace-original", displayName: "desktop-workspace", locationLabel: "shared · original", serverVersion: "0.0.1", state: "ready" };
const second: WorkspaceSummary = { ...first, id: "workspace-polish", locationLabel: "shared · polish" };

afterEach(cleanup);

function fixture(workspaces: WorkspaceSummary[], recentWorkspaces: RecentWorkspaceSummary[] = []) {
  const onSelect = vi.fn();
  const onOpenRecent = vi.fn();
  const onClose = vi.fn();
  render(<LocaleProvider><WorkspaceSwitcher workspaces={workspaces} recentWorkspaces={recentWorkspaces}
    activeWorkspaceId={workspaces[0]?.id} busy={false} onSelect={onSelect} onOpenRecent={onOpenRecent}
    onChoose={() => undefined} onClose={onClose} /></LocaleProvider>);
  return { onSelect, onOpenRecent, onClose };
}

describe("workspace label presentation", () => {
  it("disambiguates current, select and close actions using safe native ancestor labels", async () => {
    const user = userEvent.setup();
    const { onSelect, onClose } = fixture([first, second]);
    await user.click(screen.getByRole("button", { name: "Switch workspace: desktop-workspace · shared · original" }));
    expect(screen.getByText("shared · original")).toBeTruthy();
    expect(screen.getByText("shared · polish")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "desktop-workspace · shared · polish" }));
    expect(onSelect).toHaveBeenCalledWith(second.id);
    await user.click(screen.getByRole("button", { name: "Switch workspace: desktop-workspace · shared · original" }));
    await user.click(screen.getByRole("button", { name: "Close desktop-workspace · shared · original" }));
    expect(onClose).toHaveBeenCalledWith(first.id);
    expect(screen.queryByText(first.id)).toBeNull();
    expect(screen.queryByText(second.id)).toBeNull();
  });

  it("also distinguishes a closed recent workspace and reopens its exact existing identity", async () => {
    const user = userEvent.setup();
    const recent = { id: second.id, displayName: second.displayName, locationLabel: second.locationLabel, isOpen: false };
    const { onOpenRecent } = fixture([first], [recent]);
    await user.click(screen.getByRole("button", { name: /Switch workspace:/ }));
    await user.click(screen.getByRole("button", { name: "desktop-workspace · shared · polish" }));
    expect(onOpenRecent).toHaveBeenCalledWith(recent);
  });

  it("does not add location noise for a unique name or count one open/recent identity twice", async () => {
    const user = userEvent.setup();
    fixture([first], [{ id: first.id, displayName: first.displayName, locationLabel: first.locationLabel, isOpen: true }]);
    await user.click(screen.getByRole("button", { name: "Switch workspace: desktop-workspace" }));
    expect(screen.queryByText(first.locationLabel!)).toBeNull();
    expect(screen.getByRole("button", { name: "desktop-workspace" })).toBeTruthy();
  });

  it("keeps old summaries without a location label readable without inventing a path or hash", async () => {
    const user = userEvent.setup();
    fixture([{ ...first, locationLabel: undefined }, { ...second, locationLabel: undefined }]);
    await user.click(screen.getByRole("button", { name: "Switch workspace: desktop-workspace" }));
    expect(screen.getAllByRole("button", { name: "desktop-workspace" })).toHaveLength(2);
    expect(screen.queryByText(/workspace-original|workspace-polish|undefined/)).toBeNull();
  });
});
