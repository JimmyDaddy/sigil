import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ThemeProvider } from "../../appearance/ThemeProvider";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, ProviderSetupCatalog } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { SettingsPage } from "./SettingsPage";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(() => { cleanup(); window.localStorage.clear(); });

const inventory: ProviderConnectionInventory = {
  configMode: "v2", issues: [], defaultModel: { connectionId: "local", modelId: "original" },
  connections: [{ id: "local", label: "Local", providerLabel: "Local", protocolLabel: "Responses",
    endpointDisplay: "provider.example", credentialSource: "stored", readiness: "ready" }],
};

function setup(overrides: Partial<DesktopBridge> = {}, workspaceId: string | null = "navigation-workspace") {
  const updateState = vi.fn(async () => ({ phase: "unsupported", channel: "beta", currentVersion: "test", downloadedBytes: 0 }));
  const bridge = { updateState, subscribeUpdate: async () => () => undefined,
    subscribeAppearance: async () => () => undefined, ...overrides } as unknown as DesktopBridge;
  render(<LocaleProvider><ThemeProvider bridge={bridge}><NotificationProvider>
    <SettingsPage bridge={bridge} workspaceId={workspaceId ?? undefined} supportAvailable={false}
      isWorkspaceActive={() => true} providerInventory={inventory}
      onProviderInventoryChange={() => true} onProviderConfigurationReloaded={async () => undefined}
      onDefaultModelChange={() => undefined} onBack={() => undefined} onOpenSupport={() => undefined} />
  </NotificationProvider></ThemeProvider></LocaleProvider>);
  return { updateState };
}

describe("settings categories", () => {
  it("exposes four named categories while preserving the same unsaved model editor", async () => {
    const user = userEvent.setup();
    setup();
    const navigation = screen.getByRole("navigation", { name: "Settings categories" });
    expect(within(navigation).getAllByRole("button")).toHaveLength(4);
    const connections = screen.getByRole("button", { name: "Connections & models" });
    const model = screen.getByLabelText("Model ID") as HTMLInputElement;
    await user.clear(model);
    await user.type(model, "unsaved-exact-model");
    await user.click(screen.getByRole("button", { name: "Appearance & preferences" }));
    expect(screen.getByRole("heading", { name: "Appearance" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Provider connections" })).toBeNull();
    expect(screen.getByRole("region", { name: "Appearance & preferences" }).id)
      .toBe(screen.getByRole("button", { name: "Appearance & preferences" }).getAttribute("aria-controls"));
    await user.click(screen.getByRole("button", { name: "Support" }));
    expect(await screen.findByRole("heading", { name: "Desktop updates" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Support & diagnostics" })).toBeTruthy();
    expect(screen.getByText("Review diagnostics or save a private report when you need help.")).toBeTruthy();
    expect(screen.queryByText(/The setting above chooses the model/)).toBeNull();
    expect(screen.getByRole("button", { name: "Open support and diagnostics" }).hasAttribute("disabled")).toBe(true);
    await user.click(screen.getByRole("button", { name: "Extensions" }));
    expect(screen.getByRole("button", { name: "Choose JSON file" })).toBeTruthy();
    await user.click(connections);
    expect(connections.getAttribute("aria-current")).toBe("page");
    expect(screen.getByLabelText("Model ID")).toBe(model);
    expect(model.value).toBe("unsaved-exact-model");
  });

  it("preserves provider authentication drafts across categories and returns Cancel to its trigger", async () => {
    const user = userEvent.setup();
    setup();
    const add = screen.getByRole("button", { name: "Add connection" });
    await user.click(add);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    const key = screen.getByLabelText("API key") as HTMLInputElement;
    await user.type(key, "fixture-unsaved-key");
    await user.click(screen.getByRole("button", { name: "Appearance & preferences" }));
    expect(screen.queryByRole("heading", { name: "Add connection" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connections & models" }));
    expect(screen.getByLabelText("API key")).toBe(key);
    expect(key.value).toBe("fixture-unsaved-key");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("heading", { name: "Add connection" })).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(add));
  });

  it("keeps extension import selection and update subscriptions mounted when navigating", async () => {
    const user = userEvent.setup();
    const pickMcpImport = vi.fn(async () => ({ previewId: "exact-preview", rootFieldsIgnored: false,
      candidates: [{ index: 0, name: "fixture-server", transport: "stdio", description: "Local fixture",
        importable: true, issues: [] }] }));
    const { updateState } = setup({ pickMcpImport });
    await user.click(screen.getByRole("button", { name: "Extensions" }));
    await user.click(screen.getByRole("button", { name: "Choose JSON file" }));
    const selected = await screen.findByRole("checkbox", { name: "fixture-server" }) as HTMLInputElement;
    await user.click(selected);
    expect(selected.checked).toBe(false);
    await user.click(screen.getByRole("button", { name: "Support" }));
    await screen.findByText("Current version test");
    await user.click(screen.getByRole("button", { name: "Extensions" }));
    expect(screen.getByRole("checkbox", { name: "fixture-server" })).toBe(selected);
    expect(selected.checked).toBe(false);
    expect(pickMcpImport).toHaveBeenCalledTimes(1);
    expect(updateState).toHaveBeenCalledTimes(1);
  });

  it("does not move focus into a hidden category when the requested model catalog completes", async () => {
    const user = userEvent.setup();
    let resolveCatalog!: (value: ProviderSetupCatalog) => void;
    const providerSetupCatalog = vi.fn(() => new Promise<ProviderSetupCatalog>((resolve) => { resolveCatalog = resolve; }));
    setup({ providerSetupCatalog });
    await user.click(screen.getByRole("button", { name: "Add connection" }));
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "navigation-pending-key");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    await waitFor(() => expect(providerSetupCatalog).toHaveBeenCalledTimes(1));
    const preferences = screen.getByRole("button", { name: "Appearance & preferences" });
    await user.click(preferences);
    await act(async () => resolveCatalog({ connectionId: "focus-model", providerLabel: "DeepSeek", state: "remote",
      models: [{ modelId: "fixture-model", displayName: "Fixture model", availability: "available", recommended: true, provenance: "remote" }], manualEntryAllowed: true }));
    expect(document.activeElement).toBe(preferences);
    expect(screen.queryByRole("radio", { name: /Fixture model/ })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connections & models" }));
    const model = screen.getByRole("radio", { name: /Fixture model/ });
    expect(document.activeElement).toBe(model);
  });

  it("explains an extension category without a workspace instead of enabling workspace actions", async () => {
    const user = userEvent.setup();
    setup({}, null);
    await user.click(screen.getByRole("button", { name: "Extensions" }));
    expect(screen.getByText("Open a workspace to manage extensions.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Choose JSON file" })).toBeNull();
  });
});
