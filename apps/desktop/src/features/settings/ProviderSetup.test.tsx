import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, ProviderSetupCatalog } from "../../types";
import { ProviderSetup } from "./ProviderSetup";

afterEach(cleanup);

const inventory: ProviderConnectionInventory = {
  configMode: "v2",
  connections: [],
  issues: [],
};

function setup(
  providerSetupCatalog: DesktopBridge["providerSetupCatalog"],
) {
  const saveProviderSetup = vi.fn(async (
    _workspaceId: string,
    input: Parameters<DesktopBridge["saveProviderSetup"]>[1],
  ) => ({
    defaultModel: { connectionId: "saved", modelId: input.modelId },
    inventory: {
      ...inventory,
      defaultModel: { connectionId: "saved", modelId: input.modelId },
    },
    saveWarning: false,
  }));
  const bridge = { providerSetupCatalog, saveProviderSetup } as unknown as DesktopBridge;
  const onSaved = vi.fn();
  render(
    <LocaleProvider>
      <ProviderSetup bridge={bridge} workspaceId="setup-fallback-workspace"
        inventory={inventory} mode="onboarding" onSaved={onSaved} />
    </LocaleProvider>,
  );
  return { saveProviderSetup, onSaved };
}

describe("provider setup when catalog discovery fails", () => {
  it("accepts an exact model ID after a catalog RPC error and keeps the required API key", async () => {
    const user = userEvent.setup();
    const providerSetupCatalog = vi.fn().mockRejectedValue(new Error("discovery unavailable"));
    const { saveProviderSetup, onSaved } = setup(providerSetupCatalog);

    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    expect((screen.getByRole("button", { name: "Continue to models" }) as HTMLButtonElement).disabled)
      .toBe(true);
    await user.type(screen.getByLabelText("API key"), "test-secret");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));

    expect((await screen.findByRole("alert")).textContent).toContain("Models could not be loaded");
    expect(screen.getByRole("radio", { name: /Enter a model ID/ })).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save and continue" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    await user.type(screen.getByLabelText("Model ID"), "custom-coder");
    expect(save.disabled).toBe(false);
    await user.click(save);

    await waitFor(() => expect(saveProviderSetup).toHaveBeenCalledWith(
      "setup-fallback-workspace",
      expect.objectContaining({
        template: "deep_seek",
        credentialSource: "secure_store",
        apiKey: "test-secret",
        modelId: "custom-coder",
      }),
    ));
    expect(onSaved).toHaveBeenCalledOnce();
  });

  it("requires a custom endpoint and retains the manual ID across catalog retry", async () => {
    const user = userEvent.setup();
    const catalog: ProviderSetupCatalog = {
      connectionId: "custom",
      providerLabel: "OpenAI-compatible",
      state: "remote",
      models: [{
        modelId: "listed-model",
        displayName: "Listed Model",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      manualEntryAllowed: true,
    };
    const providerSetupCatalog = vi.fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue(catalog);
    const { saveProviderSetup } = setup(providerSetupCatalog);

    await user.click(screen.getByRole("button", { name: /OpenAI-compatible/ }));
    await user.selectOptions(screen.getByLabelText("Authentication"), "none");
    expect((screen.getByRole("button", { name: "Continue to models" }) as HTMLButtonElement).disabled)
      .toBe(true);
    await user.type(screen.getByLabelText("API endpoint"), "http://127.0.0.1:11434/v1");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    await screen.findByRole("radio", { name: /Enter a model ID/ });
    await user.type(screen.getByLabelText("Model ID"), "exact-local-model");
    await user.click(screen.getByRole("button", { name: "Retry refresh" }));

    await screen.findByRole("radio", { name: /Listed Model/ });
    expect((screen.getByRole("radio", { name: /Enter a model ID/ }) as HTMLInputElement).checked)
      .toBe(true);
    expect((screen.getByLabelText("Model ID") as HTMLInputElement).value)
      .toBe("exact-local-model");
    await user.click(screen.getByRole("button", { name: "Save and continue" }));
    await waitFor(() => expect(saveProviderSetup).toHaveBeenCalledWith(
      "setup-fallback-workspace",
      expect.objectContaining({
        template: "open_ai_compatible",
        endpoint: "http://127.0.0.1:11434/v1",
        credentialSource: "none",
        modelId: "exact-local-model",
      }),
    ));
  });

  it("still accepts a catalog response with rejected authentication", async () => {
    const user = userEvent.setup();
    setup(async () => ({
      connectionId: "deepseek",
      providerLabel: "DeepSeek",
      state: "auth_rejected",
      models: [],
      manualEntryAllowed: false,
    }));

    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "rejected-key");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    expect((await screen.findByRole("alert")).textContent).toContain("rejected this API key");
    expect(screen.getByRole("radio", { name: /Enter a model ID/ })).toBeTruthy();
  });
});
