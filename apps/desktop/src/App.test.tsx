import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import type { DesktopBridge } from "./bridge";
import type { CatalogPage, WorkspaceSummary } from "./types";

afterEach(cleanup);

const workspace: WorkspaceSummary = {
  id: "workspace-0123456789ab",
  displayName: "sigil",
  serverVersion: "0.0.1-alpha.5",
  state: "ready",
};

const emptyCatalog: CatalogPage = {
  workspaceId: workspace.id,
  generation: 1,
  reconciledAtUnixMs: 1_784_419_200_000,
  degradedSourceCount: 0,
  identityConflictCount: 0,
  truncatedSourceCount: 0,
  entries: [],
};

function bridgeWith(overrides: Partial<DesktopBridge> = {}): DesktopBridge {
  return {
    bootstrap: async () => ({
      protocolVersion: 1,
      workspaces: [],
      recentWorkspaces: [],
    }),
    pickWorkspace: async () => ({ cancelled: false, workspace }),
    openRecentWorkspace: async () => workspace,
    closeWorkspace: async () => [],
    catalog: async () => emptyCatalog,
    createSession: async () => ({
      id: "http-session-new",
      label: "New conversation",
      runCount: 0,
    }),
    openSession: async () => ({
      id: "http-session-open",
      label: "Durable session",
      runCount: 2,
    }),
    ...overrides,
  };
}

describe("desktop workspace and history shell", () => {
  it("renders the honest empty and recent-workspace states after native bootstrap", async () => {
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: false },
        ],
      }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("No server is running.")).toBeTruthy();
    expect(screen.getByText("Reopen")).toBeTruthy();
    expect(screen.getByText("Choose a workspace to begin.")).toBeTruthy();
  });

  it("opens a workspace, creates a conversation, and closes the owned server", async () => {
    const user = userEvent.setup();
    let closeRequest = "";
    const bridge = bridgeWith({
      closeWorkspace: async (workspaceId) => {
        closeRequest = workspaceId;
        return [];
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No server is running.");
    await user.click(screen.getAllByRole("button", { name: "Choose workspace" })[0]);
    expect(await screen.findByText("Conversation history")).toBeTruthy();
    expect(await screen.findByText("No matching conversation.")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(await screen.findByText("Conversation ready")).toBeTruthy();
    expect(screen.getByText("0 existing runs")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    expect(await screen.findByText("Workspace server closed.")).toBeTruthy();
    expect(closeRequest).toBe(workspace.id);
  });

  it("pages generation-consistent history and opens only a ready durable entry", async () => {
    const user = userEvent.setup();
    const cursors: Array<string | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: true },
        ],
      }),
      catalog: async (_workspaceId, request) => {
        cursors.push(request.cursor);
        return request.cursor === undefined
          ? {
              ...emptyCatalog,
              entries: [
                {
                  sessionRef: "first.jsonl",
                  sessionId: "durable-first",
                  sourceState: "ready",
                  sourceModifiedAtUnixMs: 1_784_419_200_000,
                  providerName: "deepseek",
                  modelName: "deepseek-chat",
                  title: "First session",
                  userMessageCount: 3,
                  assistantMessageCount: 3,
                  toolResultCount: 1,
                  pinned: false,
                },
              ],
              nextCursor: "cursor-2",
            }
          : {
              ...emptyCatalog,
              entries: [
                {
                  sessionRef: "legacy.jsonl",
                  sourceState: "unsupported_legacy",
                  sourceModifiedAtUnixMs: 1_784_419_100_000,
                  title: "Legacy session",
                  userMessageCount: 0,
                  assistantMessageCount: 0,
                  toolResultCount: 0,
                  pinned: false,
                },
              ],
            };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("First session")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByText("Legacy session")).toBeTruthy();
    expect(cursors).toEqual([undefined, "cursor-2"]);
    expect(
      (screen.getByRole("button", { name: "Inspect only" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    await user.click(screen.getByRole("button", { name: "Open" }));
    await waitFor(() => expect(screen.getByText("2 existing runs")).toBeTruthy());
  });

  it("shows a stale pagination recovery instead of mixing generations", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async (_workspaceId, request) => {
        if (request.cursor !== undefined) throw { code: "catalog_stale" };
        return { ...emptyCatalog, nextCursor: "stale-cursor" };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByText("History changed while paging.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh history" })).toBeTruthy();
  });
});
