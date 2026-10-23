import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { ProviderSetupSaveResult } from "../../types";
import { ProviderCredentialForm } from "./ProviderCredentialForm";

afterEach(() => { cleanup(); window.localStorage.clear(); });
const result: ProviderSetupSaveResult = { defaultModel: { connectionId: "default", modelId: "model" },
  inventory: { configMode: "v2", connections: [], issues: [] }, saveWarning: false };

it("keeps the credential after failure and retries the same existing connection without adding a route", async () => {
  const updateProviderConnectionCredential = vi.fn().mockRejectedValueOnce(new Error("unavailable")).mockResolvedValue(result);
  const saveProviderSetup = vi.fn();
  const onSaved = vi.fn();
  const user = userEvent.setup();
  render(<LocaleProvider><ProviderCredentialForm bridge={{ updateProviderConnectionCredential, saveProviderSetup } as unknown as DesktopBridge}
    workspaceId="workspace" connectionId="existing-other" connectionLabel="Team" active onSaved={onSaved} onCancel={vi.fn()} /></LocaleProvider>);
  const input = screen.getByLabelText("API key") as HTMLInputElement;
  expect(document.activeElement).toBe(input);
  expect(input.type).toBe("password");
  await user.type(input, "fixture-secret");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(input.value).toBe("fixture-secret");
  expect(onSaved).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(updateProviderConnectionCredential).toHaveBeenNthCalledWith(2, "workspace", { connectionId: "existing-other", apiKey: "fixture-secret" });
  expect(onSaved).toHaveBeenCalledWith(result);
  expect(input.value).toBe("");
  expect(saveProviderSetup).not.toHaveBeenCalled();
});

it("does not apply a late saved credential to a page that has left its workspace", async () => {
  let finish!: (value: ProviderSetupSaveResult) => void;
  const updateProviderConnectionCredential = vi.fn(() => new Promise((resolve) => { finish = resolve; }));
  const onSaved = vi.fn();
  const user = userEvent.setup();
  const { unmount } = render(<LocaleProvider><ProviderCredentialForm bridge={{ updateProviderConnectionCredential } as unknown as DesktopBridge}
    workspaceId="old-workspace" connectionId="existing" connectionLabel="Team" active onSaved={onSaved} onCancel={vi.fn()} /></LocaleProvider>);
  await user.type(screen.getByLabelText("API key"), "fixture-secret");
  await user.click(screen.getByRole("button", { name: "Save" }));
  unmount();
  await act(async () => finish(result));
  expect(onSaved).not.toHaveBeenCalled();
});

it("distinguishes a saved credential from a failed workspace reload and does not offer to save it again", async () => {
  const updateProviderConnectionCredential = vi.fn().mockRejectedValue({ code: "credential_saved_workspace_reload_failed" });
  const onOpenSupport = vi.fn();
  const onSaved = vi.fn();
  const user = userEvent.setup();
  render(<LocaleProvider><ProviderCredentialForm bridge={{ updateProviderConnectionCredential } as unknown as DesktopBridge}
    workspaceId="workspace" connectionId="existing" connectionLabel="Team" active onSaved={onSaved} onCancel={vi.fn()} onOpenSupport={onOpenSupport} /></LocaleProvider>);
  await user.type(screen.getByLabelText("API key"), "fixture-secret");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect((await screen.findByRole("alert")).textContent).toContain("credential was saved");
  expect(screen.queryByLabelText("API key")).toBeNull();
  expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Open support and diagnostics" }));
  expect(onOpenSupport).toHaveBeenCalledOnce();
  expect(onSaved).not.toHaveBeenCalled();
  expect(updateProviderConnectionCredential).toHaveBeenCalledOnce();
});
