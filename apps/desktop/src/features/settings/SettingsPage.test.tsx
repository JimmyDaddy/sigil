import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ThemeProvider } from "../../appearance/ThemeProvider";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, RunContext } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { SettingsPage } from "./SettingsPage";
import type { ProviderRecoverySettingsContext } from "../conversation/providerRecovery";

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

function settings(context?: RunContext, connections = inventory.connections, recoveryContext?: ProviderRecoverySettingsContext) {
  const saveProviderDefaultModel = vi.fn(async (
    _workspaceId: string,
    modelRef: { connectionId: string; modelId: string },
  ) => ({ defaultModel: modelRef, inventory, saveWarning: false }));
  const updateProviderConnectionCredential = vi.fn(async () => ({ defaultModel: inventory.defaultModel!, inventory, saveWarning: false }));
  const bridge = {
    updateState: async () => ({ phase: "unsupported", channel: "beta", currentVersion: "test", downloadedBytes: 0 }),
    subscribeUpdate: async () => () => undefined,
    subscribeAppearance: async () => () => undefined,
    saveProviderDefaultModel,
    updateProviderConnectionCredential,
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
            providerInventory={{ ...inventory, connections }}
            modelContext={context}
            defaultModel={context?.modelRef ?? inventory.defaultModel}
            recoveryContext={recoveryContext}
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
  return { saveProviderDefaultModel, updateProviderConnectionCredential };
}

it.each(["stored", "environment", "none"] as const)("keeps the failed connection distinct from the default and explains its current %s authentication", (source) => {
  const connections = inventory.connections.map((connection) => connection.id === "team-route"
    ? { ...connection, credentialSource: source } : connection);
  const actions = settings(undefined, connections, { runId: "failed-run", reasonCode: "provider_authentication_required",
    failedModelRef: { connectionId: "team-route", modelId: "failed-model" } });
  const note = screen.getByText("Connection authentication failed").closest("[role=status]")!;
  expect(note.textContent).toContain("The failed request used Team gateway · failed-model.");
  expect(note.textContent).toContain(source === "none" ? "Check whether the service requires it."
    : source === "environment" ? "environment credential used to start this workspace" : "update its authentication information");
  expect(screen.getAllByText("Configured")).toHaveLength(2);
  expect(screen.queryByLabelText("API key")).toBeNull();
  expect(actions.updateProviderConnectionCredential).not.toHaveBeenCalled();
  expect(actions.saveProviderDefaultModel).not.toHaveBeenCalled();
});

it.each([undefined, { connectionId: "deleted-connection", modelId: "model" }])("does not substitute the default when the failed route cannot be located (%s)", (failedModelRef) => {
  const actions = settings(undefined, inventory.connections, { runId: "unresolved-run", reasonCode: "provider_authentication_required", failedModelRef });
  expect(screen.getByText(failedModelRef === undefined
    ? "This request has no identifiable connection. Choose the connection you want to check."
    : "The connection used by this request is no longer available. Choose another connection.")).toBeTruthy();
  expect(screen.queryByText(/The failed request used DeepSeek/)).toBeNull();
  expect(screen.queryByLabelText("API key")).toBeNull();
  expect(actions.updateProviderConnectionCredential).not.toHaveBeenCalled();
  expect(actions.saveProviderDefaultModel).not.toHaveBeenCalled();
});

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

describe("existing connection credential management", () => {
  it("lets a ready saved-key connection rotate its key by exact ID and returns Cancel focus", async () => {
    const user = userEvent.setup();
    const { updateProviderConnectionCredential, saveProviderDefaultModel } = settings();
    const trigger = screen.getByRole("button", { name: "Manage DeepSeek" });
    expect(screen.queryByRole("button", { name: "Update credential for DeepSeek" })).toBeNull();
    trigger.focus();
    await user.keyboard("{Enter}");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Update credential" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(trigger);
    await user.keyboard("{Enter}");
    await user.keyboard("{Enter}");
    const input = await screen.findByLabelText("API key");
    await waitFor(() => expect(document.activeElement).toBe(input));
    await user.type(input, "not-submitted-key");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(document.activeElement).toBe(trigger));
    expect(updateProviderConnectionCredential).not.toHaveBeenCalled();
    await user.click(trigger);
    await user.click(screen.getByRole("menuitem", { name: "Update credential" }));
    expect((await screen.findByLabelText("API key") as HTMLInputElement).value).toBe("");
    await user.type(screen.getByLabelText("API key"), "fixture-replacement-key");
    await user.click(within(screen.getByRole("form", { name: "Update credential for DeepSeek" })).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(updateProviderConnectionCredential).toHaveBeenCalledExactlyOnceWith(
      "fixture-workspace", { connectionId: "deepseek-default", apiKey: "fixture-replacement-key" },
    ));
    await waitFor(() => expect(document.activeElement).toBe(trigger));
    expect(saveProviderDefaultModel).not.toHaveBeenCalled();
  });

  it.each(["ready", "unverified"] as const)("makes replacing a %s environment reference explicit and does not add authentication to a none connection", async (readiness) => {
    const user = userEvent.setup();
    const { updateProviderConnectionCredential } = settings(undefined, [...inventory.connections.map((connection) => ({ ...connection, readiness })), {
      ...inventory.connections[0], id: "local-none", label: "Local server", credentialSource: "none",
    }]);
    expect(screen.queryByRole("button", { name: "Update credential for Local server" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Manage Local server" })).toBeNull();
    const environment = screen.getByRole("button", { name: "Manage Team gateway" });
    await user.click(environment);
    await user.click(screen.getByRole("menuitem", { name: "Use a saved API key" }));
    expect(await screen.findByText("This connection will use the new saved API key instead of its environment variable.")).toBeTruthy();
    await user.type(screen.getByLabelText("API key"), "fixture-environment-replacement");
    await user.click(within(screen.getByRole("form", { name: "Update credential for Team gateway" })).getByRole("button", { name: "Use a saved API key" }));
    await waitFor(() => expect(updateProviderConnectionCredential).toHaveBeenCalledExactlyOnceWith(
      "fixture-workspace", { connectionId: "team-route", apiKey: "fixture-environment-replacement" },
    ));
  });

  it.each(["needs_credential", "credential_unavailable"] as const)("keeps %s repair visible without a management-menu detour", async (readiness) => {
    const user = userEvent.setup();
    settings(undefined, [{ ...inventory.connections[0], readiness }]);
    expect(screen.queryByRole("button", { name: "Manage DeepSeek" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Update credential for DeepSeek" }));
    expect(document.activeElement).toBe(screen.getByLabelText("API key"));
  });
});
