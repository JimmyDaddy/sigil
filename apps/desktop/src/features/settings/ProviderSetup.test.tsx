import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderConnectionInventory, ProviderSetupCatalog } from "../../types";
import { ProviderSetup } from "./ProviderSetup";
import { providerEndpointIssue } from "./providerEndpoint";

afterEach(cleanup);

it.each([
  ["not-a-url", "none", "providerEndpointInvalid"],
  ["file:///tmp/config", "none", "providerEndpointInvalid"],
  ["https://user:secret@example.test/v1", "secure_store", "providerEndpointUserinfo"],
  ["https://example.test/v1?key=secret", "secure_store", "providerEndpointSuffix"],
  ["https://example.test/v1#fragment", "secure_store", "providerEndpointSuffix"],
  ["http://example.test/v1", "secure_store", "providerEndpointHttps"],
  ["https://example.test/v1", "none", "providerEndpointLoopback"],
  ["http://127.0.0.1:60433/v1", "none", undefined],
  ["https://example.test/v1", "environment", undefined],
] as const)("validates endpoint structure without fetching: %s (%s)", (endpoint, source, issue) => {
  expect(providerEndpointIssue(endpoint, source)).toBe(issue);
});

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
  it("keeps an invalid endpoint beside its correction and retains the authentication draft", async () => {
    const user = userEvent.setup();
    const catalog = vi.fn().mockRejectedValue(new Error("offline"));
    setup(catalog);
    await user.click(screen.getByRole("button", { name: /OpenAI-compatible/ }));
    await user.type(screen.getByLabelText("API endpoint"), "not-a-url");
    await user.type(screen.getByLabelText("API key"), "retained-key");
    await user.keyboard("{Enter}");
    expect(screen.getByText("Enter a complete http:// or https:// address.")).toBeTruthy();
    expect(screen.getByLabelText("API endpoint").getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByLabelText("API key").getAttribute("type")).toBe("password");
    expect((screen.getByLabelText("API key") as HTMLInputElement).value).toBe("retained-key");
    expect(catalog).not.toHaveBeenCalled();
    await user.clear(screen.getByLabelText("API endpoint"));
    await user.type(screen.getByLabelText("API endpoint"), "https://example.test/v1");
    expect(screen.queryByText("Enter a complete http:// or https:// address.")).toBeNull();
    await user.keyboard("{Enter}");
    expect(await screen.findByLabelText("Model ID")).toBeTruthy();
    expect(catalog).toHaveBeenCalledOnce();
  });

  it.each(["during", "end-before-enter"] as const)("preserves native IME confirmation and advances on the next Enter (%s)", async (ordering) => {
    const user = userEvent.setup();
    const catalog = vi.fn().mockRejectedValue(new Error("offline"));
    setup(catalog);
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    const key = screen.getByLabelText("API key");
    await user.type(key, "fixture-key");
    fireEvent.compositionStart(key);
    if (ordering === "end-before-enter") fireEvent.compositionEnd(key, { data: "你" });
    expect(fireEvent.keyDown(key, { key: "Enter", keyCode: ordering === "during" ? 229 : 13,
      isComposing: ordering === "during" })).toBe(true);
    expect(catalog).not.toHaveBeenCalled();
    if (ordering === "during") fireEvent.compositionEnd(key, { data: "你" });
    fireEvent.keyUp(key, { key: "Enter" });
    fireEvent.keyDown(key, { key: "Enter", keyCode: 13 });
    await screen.findByLabelText("Model ID");
    expect(catalog).toHaveBeenCalledOnce();
  });

  it("does not advance on modified Enter and explains a missing required API key", async () => {
    const user = userEvent.setup();
    const catalog = vi.fn();
    setup(catalog);
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    const key = screen.getByLabelText("API key");
    fireEvent.keyDown(key, { key: "Enter", shiftKey: true });
    expect(screen.queryByText("Enter an API key or choose another authentication method.")).toBeNull();
    fireEvent.keyDown(key, { key: "Enter" });
    expect(screen.getByText("Enter an API key or choose another authentication method.")).toBeTruthy();
    expect(catalog).not.toHaveBeenCalled();
  });
  it("focuses and scrolls the first control on user-driven step changes", async () => {
    const user = userEvent.setup();
    const original = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
    try {
      setup(vi.fn());
      const provider = screen.getByRole("button", { name: /DeepSeek/ });
      expect(document.activeElement).toBe(provider);
      expect(scrollIntoView.mock.contexts.at(-1)).toBe(provider);
      await user.click(provider);
      expect(document.activeElement).toBe(screen.getByLabelText("Authentication"));
      expect(scrollIntoView.mock.contexts.at(-1)).toBe(screen.getByLabelText("Authentication"));
      await user.click(screen.getByRole("button", { name: "Back" }));
      expect(document.activeElement).toBe(screen.getByRole("button", { name: /DeepSeek/ }));
    } finally {
      if (original === undefined) Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
      else Object.defineProperty(HTMLElement.prototype, "scrollIntoView", original);
    }
  });

  it("keeps focus and manual input intact when a same-step catalog retry resolves", async () => {
    const user = userEvent.setup();
    let resolveCatalog!: (value: ProviderSetupCatalog) => void;
    setup(vi.fn().mockRejectedValueOnce(new Error("offline"))
      .mockImplementationOnce(() => new Promise<ProviderSetupCatalog>((resolve) => { resolveCatalog = resolve; })));
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "focus-refresh-key");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    const model = await screen.findByLabelText("Model ID") as HTMLInputElement;
    await user.click(screen.getByRole("button", { name: "Retry refresh" }));
    await user.type(model, "exact-model-draft");
    const caret = model.selectionStart;
    await act(async () => resolveCatalog({ connectionId: "retry", providerLabel: "DeepSeek", state: "remote",
      models: [{ modelId: "listed", displayName: "Listed after retry", availability: "available", recommended: true, provenance: "remote" }],
      manualEntryAllowed: true }));
    expect(await screen.findByRole("radio", { name: /Listed after retry/ })).toBeTruthy();
    expect(document.activeElement).toBe(model);
    expect(model.value).toBe("exact-model-draft");
    expect(model.selectionStart).toBe(caret);
  });

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
