import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { ThemeProvider } from "../../appearance/ThemeProvider";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import { NotificationProvider } from "../../ui/feedback";
import { SettingsPage } from "./SettingsPage";
import { useSettingsNavigation } from "./settingsNavigation";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(() => { cleanup(); window.localStorage.clear(); });

const bridge = {
  updateState: async () => ({ phase: "unsupported", channel: "beta", currentVersion: "test", downloadedBytes: 0 }),
  subscribeUpdate: async () => () => undefined,
  subscribeAppearance: async () => () => undefined,
} as unknown as DesktopBridge;

function Harness({ workspaceId }: { readonly workspaceId: string }) {
  const [support, setSupport] = useState(false);
  const navigation = useSettingsNavigation(workspaceId);
  return <LocaleProvider><ThemeProvider bridge={bridge}><NotificationProvider>
    {support ? <button type="button" onClick={() => setSupport(false)}>Return from diagnostics</button> :
      <SettingsPage key={workspaceId} bridge={bridge} workspaceId={workspaceId} supportAvailable
        isWorkspaceActive={() => true} providerInventory={{ configMode: "v2", connections: [], issues: [] }}
        onProviderInventoryChange={() => true} onProviderConfigurationReloaded={async () => undefined}
        onDefaultModelChange={() => undefined} onBack={() => undefined} onOpenSupport={() => setSupport(true)}
        navigationState={navigation.value} onNavigationStateChange={navigation.remember} />}
  </NotificationProvider></ThemeProvider></LocaleProvider>;
}

it("restores the support category, scroll position and initiating control after the settings page remounts", async () => {
  const user = userEvent.setup();
  render(<Harness workspaceId="first" />);
  await user.click(screen.getByRole("button", { name: "Support" }));
  const sections = document.querySelector<HTMLDivElement>(".settings-sections")!;
  sections.scrollTop = 218;
  fireEvent.scroll(sections);
  await user.click(screen.getByRole("button", { name: "Open support and diagnostics" }));
  expect(screen.queryByRole("heading", { name: "Settings" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Return from diagnostics" }));
  expect(screen.getByRole("button", { name: "Support" }).getAttribute("aria-current")).toBe("page");
  expect(document.querySelector<HTMLDivElement>(".settings-sections")!.scrollTop).toBe(218);
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Open support and diagnostics" }));
});

it("restores each workspace's category independently", async () => {
  const user = userEvent.setup();
  const { rerender } = render(<Harness workspaceId="first" />);
  await user.click(screen.getByRole("button", { name: "Appearance & preferences" }));
  rerender(<Harness workspaceId="second" />);
  expect(screen.getByRole("button", { name: "Connections & models" }).getAttribute("aria-current")).toBe("page");
  await user.click(screen.getByRole("button", { name: "Support" }));
  rerender(<Harness workspaceId="first" />);
  expect(screen.getByRole("button", { name: "Appearance & preferences" }).getAttribute("aria-current")).toBe("page");
});

it("does not replace existing focus when the remembered entry is unavailable", () => {
  render(<LocaleProvider><ThemeProvider bridge={bridge}><NotificationProvider>
    <button type="button" autoFocus>Existing focus</button>
    <SettingsPage bridge={bridge} workspaceId="first" supportAvailable={false}
      isWorkspaceActive={() => true} providerInventory={{ configMode: "v2", connections: [], issues: [] }}
      onProviderInventoryChange={() => true} onProviderConfigurationReloaded={async () => undefined}
      onDefaultModelChange={() => undefined} onBack={() => undefined} onOpenSupport={() => undefined}
      navigationState={{ category: "support", scrollTop: 0, focusKey: "open-support" }} />
  </NotificationProvider></ThemeProvider></LocaleProvider>);
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Existing focus" }));
});
