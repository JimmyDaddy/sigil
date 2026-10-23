import { useState } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ThemeProvider } from "../../appearance/ThemeProvider";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, ProviderModelRef } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { SettingsPage } from "./SettingsPage";
import { useContextWindowDrafts } from "./useContextWindowDrafts";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(() => { cleanup(); window.localStorage.clear(); });

const model = { connectionId: "local", modelId: "first" };
const inventory: ProviderConnectionInventory = {
  configMode: "v2", issues: [], defaultModel: model,
  connections: ["local", "other"].map((id) => ({ id, label: id, providerLabel: "Custom",
    protocolLabel: "Chat Completions", endpointDisplay: "fixture.example", credentialSource: "none", readiness: "ready" })),
};

function setup(failSave = false, saveDelay?: Promise<void>) {
  const saveProviderDefaultModel = vi.fn(async (_workspace: string, modelRef: ProviderModelRef, tokens?: number) => {
    await saveDelay;
    if (failSave) throw new Error("fixture-save-failed");
    return { defaultModel: modelRef, inventory: { ...inventory, defaultModel: modelRef,
      connections: inventory.connections.map((connection) => connection.id === modelRef.connectionId
        ? { ...connection, modelContextWindows: tokens === undefined ? {} : { [modelRef.modelId]: tokens } }
        : connection) }, saveWarning: false };
  });
  const bridge = { saveProviderDefaultModel,
    updateState: async () => ({ phase: "unsupported", channel: "beta", currentVersion: "test", downloadedBytes: 0 }),
    subscribeUpdate: async () => () => undefined, subscribeAppearance: async () => () => undefined,
  } as unknown as DesktopBridge;
  function DraftOwner() {
    const drafts = useContextWindowDrafts();
    const [open, setOpen] = useState(true);
    const [workspaceId, setWorkspaceId] = useState("workspace-a");
    const [modelRef, setModelRef] = useState(model);
    const [current, setCurrent] = useState(inventory);
    return <>
      <button onClick={() => setOpen(true)}>Reopen settings</button>
      <button onClick={() => setWorkspaceId((id) => id === "workspace-a" ? "workspace-b" : "workspace-a")}>Switch workspace</button>
      <button onClick={() => setModelRef((ref) => ({ ...ref, connectionId: ref.connectionId === "local" ? "other" : "local" }))}>Switch connection</button>
      <button onClick={() => setModelRef((ref) => ({ ...ref, modelId: ref.modelId === "first" ? "second" : "first" }))}>Switch model</button>
      <button onClick={() => setCurrent({ ...inventory, connections: inventory.connections.map((connection) => ({ ...connection, modelContextWindows: { first: 8192 } })) })}>Refresh inventory</button>
      {open ? <SettingsPage bridge={bridge} supportAvailable={false} workspaceId={workspaceId}
        isWorkspaceActive={() => true} providerInventory={current} defaultModel={modelRef}
        contextWindowDrafts={drafts} onProviderInventoryChange={(value) => { setCurrent(value); return true; }}
        onProviderConfigurationReloaded={async () => undefined} onDefaultModelChange={(value) => { if (value !== undefined) setModelRef(value); }}
        onBack={() => setOpen(false)} onOpenSupport={() => undefined} /> : null}
    </>;
  }
  render(<LocaleProvider><ThemeProvider bridge={bridge}><NotificationProvider><DraftOwner /></NotificationProvider></ThemeProvider></LocaleProvider>);
  return { saveProviderDefaultModel };
}

describe("context window settings drafts", () => {
  it("retains unsaved edits across categories and page unmounting without saving", async () => {
    const user = userEvent.setup();
    const { saveProviderDefaultModel } = setup();
    await user.type(screen.getByLabelText("Context window"), "65536");
    expect(screen.getByText("Unsaved")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Appearance & preferences" }));
    await user.click(screen.getByRole("button", { name: "Connections & models" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    expect(screen.queryByLabelText("Context window")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Reopen settings" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
    expect(saveProviderDefaultModel).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Revert" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("");
    expect(screen.queryByText("Unsaved")).toBeNull();
  });

  it.each(["workspace", "connection", "model"])("isolates drafts by %s and preserves them over inventory refresh", async (scope) => {
    const user = userEvent.setup();
    setup();
    await user.type(screen.getByLabelText("Context window"), "65536");
    const switcher = screen.getByRole("button", { name: `Switch ${scope}` });
    await user.click(switcher);
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("");
    await user.type(screen.getByLabelText("Context window"), "32768");
    await user.click(switcher);
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
    await user.click(screen.getByRole("button", { name: "Refresh inventory" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
    expect(screen.getByText("Unsaved")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Revert" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("8192");
  });

  it("clears dirty only after explicit successful save and supports automatic context", async () => {
    const user = userEvent.setup();
    const { saveProviderDefaultModel } = setup();
    const field = screen.getByLabelText("Context window");
    await user.type(field, "65536");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenCalledExactlyOnceWith("workspace-a", model, 65536));
    await waitFor(() => expect(screen.queryByText("Unsaved")).toBeNull());
    expect((field as HTMLInputElement).value).toBe("65536");
    await user.clear(field);
    expect(screen.getByText("Unsaved")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenLastCalledWith("workspace-a", model, undefined));
    await waitFor(() => expect(screen.queryByText("Unsaved")).toBeNull());
  });

  it("retains the editable draft after a failed save and rejects invalid numbers", async () => {
    const user = userEvent.setup();
    const { saveProviderDefaultModel } = setup(true);
    const field = screen.getByLabelText("Context window");
    await user.type(field, "0");
    expect(screen.getByRole("button", { name: "Save" }).hasAttribute("disabled")).toBe(true);
    await user.clear(field);
    await user.type(field, "65536");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenCalledTimes(1));
    expect((field as HTMLInputElement).value).toBe("65536");
    expect(screen.getByText("Unsaved")).toBeTruthy();
    expect(await screen.findByText("This preference could not be saved. The previous value is still active.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    await user.click(screen.getByRole("button", { name: "Reopen settings" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
  });

  it("preserves newer edits if an earlier save completes after navigating back", async () => {
    const user = userEvent.setup();
    let finishSave!: () => void;
    const delay = new Promise<void>((resolve) => { finishSave = resolve; });
    const { saveProviderDefaultModel } = setup(false, delay);
    await user.type(screen.getByLabelText("Context window"), "65536");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenCalledTimes(1));
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    await user.click(screen.getByRole("button", { name: "Reopen settings" }));
    await user.clear(screen.getByLabelText("Context window"));
    await user.type(screen.getByLabelText("Context window"), "131072");
    finishSave();
    await screen.findByText("Context window saved for this connection and model.");
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("131072");
    expect(screen.getByText("Unsaved")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Revert" }));
    expect((screen.getByLabelText("Context window") as HTMLInputElement).value).toBe("65536");
    await user.clear(screen.getByLabelText("Context window"));
    await user.type(screen.getByLabelText("Context window"), "65536");
    expect(screen.queryByText("Unsaved")).toBeNull();
  });
});
