import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import type { DesktopBridge } from "./bridge";
import type { WorkspaceSummary } from "./types";

afterEach(cleanup);

const workspace: WorkspaceSummary = {
  id: "workspace-0123456789ab",
  displayName: "sigil",
  serverVersion: "0.0.1-alpha.5",
  state: "ready",
};

function bridgeWith(overrides: Partial<DesktopBridge> = {}): DesktopBridge {
  return {
    bootstrap: async () => ({ protocolVersion: 1, workspaces: [] }),
    pickWorkspace: async () => ({ cancelled: false, workspace }),
    closeWorkspace: async () => [],
    ...overrides,
  };
}

describe("desktop workspace shell", () => {
  it("renders the honest empty state after native bootstrap", async () => {
    render(<App bridge={bridgeWith()} />);

    expect(
      await screen.findByText("No workspace server is running."),
    ).toBeTruthy();
    expect(screen.getByText("Choose a workspace to begin.")).toBeTruthy();
  });

  it("opens and closes a backend-owned workspace through coarse actions", async () => {
    const user = userEvent.setup();
    let closeRequest = "";
    const bridge = bridgeWith({
      closeWorkspace: async (workspaceId) => {
        closeRequest = workspaceId;
        return [];
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No workspace server is running.");
    await user.click(screen.getByRole("button", { name: "Choose workspace" }));
    expect(await screen.findByText("sigil is ready.")).toBeTruthy();
    expect(screen.getByText("ready · server 0.0.1-alpha.5")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    expect(await screen.findByText("Workspace server closed.")).toBeTruthy();
    expect(closeRequest).toBe(workspace.id);
  });
});
