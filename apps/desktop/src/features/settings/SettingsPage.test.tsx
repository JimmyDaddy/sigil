import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ThemeProvider } from "../../appearance/ThemeProvider";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, RunContext } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { SettingsPage } from "./SettingsPage";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const inventory: ProviderConnectionInventory = {
  configMode: "v2",
  defaultModel: { connectionId: "deepseek-default", modelId: "flash" },
  connections: [
    {
      id: "deepseek-default",
      label: " DeepSeek ",
      providerLabel: "DeepSeek",
      protocolLabel: "DeepSeek",
      endpointDisplay: "provider.example",
      credentialSource: "stored",
      readiness: "ready",
      defaultModel: { connectionId: "deepseek-default", modelId: "flash" },
    },
    {
      id: "team-route",
      label: "Team gateway",
      providerLabel: "DeepSeek",
      protocolLabel: "Chat Completions",
      endpointDisplay: "gateway.example",
      credentialSource: "environment",
      readiness: "ready",
      defaultModel: { connectionId: "team-route", modelId: "flash" },
    },
  ],
  issues: [],
};

function modelContext(connectionId: string): RunContext {
  return {
    modelRef: { connectionId, modelId: "flash" },
    providerName: "DeepSeek",
    modelName: "flash",
    modelOptions: inventory.connections.map((connection) => ({
      modelRef: { connectionId: connection.id, modelId: "flash" },
      displayName: "flash",
      modelName: "flash",
      availability: "available",
      recommendation: "standard",
      provenance: "configured",
      availableReasoningEfforts: [],
    })),
    modelSelection: "same_session",
    modelSelectionBinding: "fixture-selection",
    defaultPermissionMode: "manual",
    availablePermissionModes: ["manual"],
    availableReasoningEfforts: [],
    contextWindowSource: "unavailable",
    extensionCatalog: { commands: [], skills: [], agents: [] },
  };
}

function settings(context?: RunContext) {
  const saveProviderDefaultModel = vi.fn(async (
    _workspaceId: string,
    modelRef: { connectionId: string; modelId: string },
  ) => ({ defaultModel: modelRef, inventory, saveWarning: false }));
  const bridge = {
    updateState: async () => ({ phase: "unsupported", channel: "beta", currentVersion: "test", downloadedBytes: 0 }),
    subscribeUpdate: async () => () => undefined,
    subscribeAppearance: async () => () => undefined,
    saveProviderDefaultModel,
  } as unknown as DesktopBridge;
  render(
    <LocaleProvider>
      <ThemeProvider bridge={bridge}>
        <NotificationProvider>
          <SettingsPage
            bridge={bridge}
            supportAvailable={false}
            workspaceId="fixture-workspace"
            isWorkspaceActive={() => true}
            providerInventory={inventory}
            modelContext={context}
            defaultModel={context?.modelRef ?? inventory.defaultModel}
            onProviderInventoryChange={() => true}
            onProviderConfigurationReloaded={async () => undefined}
            onDefaultModelChange={() => undefined}
            onBack={() => undefined}
            onOpenSupport={() => undefined}
          />
        </NotificationProvider>
      </ThemeProvider>
    </LocaleProvider>,
  );
  return { saveProviderDefaultModel };
}

describe("settings provider identity labels", () => {
  it.each(["deepseek-default", "team-route"])(
    "keeps connection and provider distinctions for the current route %s",
    async (connectionId) => {
      settings(modelContext(connectionId));
      const providerSection = screen.getByRole("heading", { name: "Provider connections" }).closest("section")!;
      const rows = within(providerSection).getAllByRole("listitem");
      expect(rows[0].textContent?.match(/DeepSeek/g)).toHaveLength(1);
      expect(rows[1].textContent).toContain("Team gateway");
      expect(rows[1].textContent).toContain("DeepSeek · Chat Completions");
      const selector = screen.getByRole("combobox", { name: "Default model for new conversations" });
      expect(within(selector).getByRole("option", { name: "DeepSeek · flash" }).getAttribute("value"))
        .toBe("deepseek-default/flash");
      expect(within(selector).getByRole("option", { name: "DeepSeek · Team gateway · flash" }).getAttribute("value"))
        .toBe("team-route/flash");
      expect(screen.queryByText(/DeepSeek · DeepSeek/)).toBeNull();
      expect(screen.getByText(connectionId === "deepseek-default"
        ? /^Connection DeepSeek\. Existing conversations/
        : /^Connection Team gateway · Provider DeepSeek\./)).toBeTruthy();
      await userEvent.setup().click(screen.getByRole("button", { name: "Support" }));
      await screen.findByText("Current version test");
    },
  );

  it("deduplicates the session-free connection picker without changing the saved route", async () => {
    const user = userEvent.setup();
    const { saveProviderDefaultModel } = settings();
    const selector = screen.getByRole("combobox", { name: "Configured connection" });
    expect(within(selector).getByRole("option", { name: "DeepSeek" }).getAttribute("value"))
      .toBe("deepseek-default");
    expect(within(selector).getByRole("option", { name: "DeepSeek · Team gateway" }).getAttribute("value"))
      .toBe("team-route");
    await user.selectOptions(selector, "team-route");
    await user.click(screen.getByRole("button", { name: "Save default model" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenCalledExactlyOnceWith(
      "fixture-workspace", { connectionId: "team-route", modelId: "flash" },
    ));
  });
});
