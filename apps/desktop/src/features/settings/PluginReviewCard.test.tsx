import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { PluginReview } from "../../types";
import { PluginReviewCard } from "./PluginReviewCard";

afterEach(cleanup);
const plugin: PluginReview = { pluginId: "review", name: "Review tools", version: "1", manifestHash: "sha256:manifest",
  capabilityDigest: "sha256:capabilities", trust: "needs_review",
  capabilities: [{ kind: "mcp", label: "browser", approval: "ask", allowSecrets: false, egressLogging: true }] };

it("uses the exact displayed declaration and retains ordinary approvals", async () => {
  const user = userEvent.setup();
  const pluginCatalog = vi.fn().mockResolvedValue({ plugins: [plugin], warningCount: 0 });
  const commandConversationRecovery = vi.fn(async () => ({ pluginReview: { pluginId: plugin.pluginId, enabled: true } }));
  const bridge = { pluginCatalog, commandConversationRecovery } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  await screen.findByText("Review tools · 1");
  expect(commandConversationRecovery).not.toHaveBeenCalled();
  expect(screen.getByText(/Approval: ask/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Trust and enable" }));
  expect(commandConversationRecovery).toHaveBeenCalledExactlyOnceWith("workspace", { sessionId: "session", action: {
    kind: "review_plugin", pluginId: plugin.pluginId, manifestHash: plugin.manifestHash,
    capabilityDigest: plugin.capabilityDigest, enabled: true,
  } });
  expect((await screen.findByRole("status")).textContent).toContain("Plugin decision saved");
});

it("rejects a mismatched receipt and lets a stale declaration be refreshed", async () => {
  const user = userEvent.setup();
  const pluginCatalog = vi.fn().mockResolvedValue({ plugins: [plugin], warningCount: 0 });
  const bridge = { pluginCatalog, commandConversationRecovery: async () => ({ pluginReview: { pluginId: "other", enabled: true } }) } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  await screen.findByText("Review tools · 1");
  await user.click(screen.getByRole("button", { name: "Trust and enable" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Refresh the declaration");
  expect(screen.queryByRole("status")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Refresh source" }));
  expect(pluginCatalog).toHaveBeenCalledTimes(2);
});

it.each(["unknown", "unconfirmed", undefined] as const)("keeps disabled cleanup retryable when its status is %s", async (processCleanup) => {
  const user = userEvent.setup();
  const current: PluginReview = { ...plugin, trust: "disabled", processCleanup };
  const pluginCatalog = vi.fn().mockResolvedValue({ plugins: [current], warningCount: 0 });
  const commandConversationRecovery = vi.fn().mockResolvedValue({ pluginReview: { pluginId: plugin.pluginId, enabled: false, processCleanup: "unconfirmed" } });
  const bridge = { pluginCatalog, commandConversationRecovery } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  const retry = await screen.findByRole("button", { name: "Retry cleanup" });
  expect((retry as HTMLButtonElement).disabled).toBe(false);
  expect(screen.getByRole("alert").textContent).toMatch(/could not be confirmed|not confirmed stopped/);
  await user.click(retry);
  expect(commandConversationRecovery).toHaveBeenCalledExactlyOnceWith("workspace", { sessionId: "session", action: {
    kind: "review_plugin", pluginId: plugin.pluginId, manifestHash: plugin.manifestHash,
    capabilityDigest: plugin.capabilityDigest, enabled: false,
  } });
  await screen.findByText(/Plugin decision saved/);
  expect((screen.getByRole("button", { name: "Retry cleanup" }) as HTMLButtonElement).disabled).toBe(false);
});

it("only clears an earlier cleanup warning with a confirmed canonical catalog", async () => {
  const user = userEvent.setup();
  const pluginCatalog = vi.fn()
    .mockResolvedValueOnce({ plugins: [{ ...plugin, trust: "disabled", processCleanup: "unconfirmed" }], warningCount: 0 })
    .mockResolvedValueOnce({ plugins: [{ ...plugin, trust: "trusted", processCleanup: null }], warningCount: 0 })
    .mockResolvedValue({ plugins: [{ ...plugin, trust: "trusted", processCleanup: "confirmed" }], warningCount: 0 });
  // This operation's confirmed receipt cannot clear an older generation's unresolved cleanup.
  const commandConversationRecovery = vi.fn().mockResolvedValue({ pluginReview: { pluginId: plugin.pluginId, enabled: true, processCleanup: "confirmed" } });
  const bridge = { pluginCatalog, commandConversationRecovery } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  await screen.findByText("Earlier plugin processes are not confirmed stopped.");
  await user.click(screen.getByRole("button", { name: "Trust and enable" }));
  await screen.findByText("Trusted");
  expect(screen.getByRole("alert").textContent).toContain("not confirmed stopped");
  expect(screen.queryByRole("button", { name: "Retry cleanup" })).toBeNull();
  expect((screen.getByRole("button", { name: "Disable" }) as HTMLButtonElement).disabled).toBe(false);
  await user.click(screen.getByRole("button", { name: "Refresh source" }));
  await screen.findByText("Earlier plugin processes are confirmed stopped.");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("does not offer redundant cleanup after canonical confirmation", async () => {
  const bridge = { pluginCatalog: async () => ({ plugins: [{ ...plugin, trust: "disabled", processCleanup: "confirmed" }], warningCount: 0 }) } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  await screen.findByText("Earlier plugin processes are confirmed stopped.");
  expect((screen.getByRole("button", { name: "Disable" }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Trust and enable" }) as HTMLButtonElement).disabled).toBe(false);
});

it("preserves cleanup uncertainty when canonical refresh fails", async () => {
  const user = userEvent.setup();
  const pluginCatalog = vi.fn()
    .mockResolvedValueOnce({ plugins: [{ ...plugin, trust: "disabled", processCleanup: "unknown" }], warningCount: 0 })
    .mockRejectedValue(new Error("catalog unavailable"));
  const bridge = { pluginCatalog, commandConversationRecovery: async () => ({ pluginReview: { pluginId: plugin.pluginId, enabled: false, processCleanup: "unconfirmed" } }) } as unknown as DesktopBridge;
  render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="session" /></LocaleProvider>);
  await user.click(await screen.findByRole("button", { name: "Retry cleanup" }));
  await screen.findByText("The review could not be completed. Refresh the declaration and retry.");
  expect(screen.getByText("Earlier plugin processes are not confirmed stopped.")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Retry cleanup" }) as HTMLButtonElement).disabled).toBe(false);
});

it("does not carry a late cleanup receipt or warning into another session", async () => {
  const user = userEvent.setup();
  let complete!: (value: unknown) => void;
  const pending = new Promise((resolve) => { complete = resolve; });
  const pluginCatalog = vi.fn(async (_workspace: string, session: string) => ({ plugins: [session === "first"
    ? { ...plugin, trust: "disabled", processCleanup: "unconfirmed" }
    : { ...plugin, trust: "trusted", processCleanup: null }], warningCount: 0 }));
  const bridge = { pluginCatalog, commandConversationRecovery: () => pending } as unknown as DesktopBridge;
  const rendered = render(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="first" /></LocaleProvider>);
  await user.click(await screen.findByRole("button", { name: "Retry cleanup" }));
  rendered.rerender(<LocaleProvider><PluginReviewCard bridge={bridge} workspaceId="workspace" sessionId="second" /></LocaleProvider>);
  await screen.findByText("Trusted");
  complete({ pluginReview: { pluginId: plugin.pluginId, enabled: false, processCleanup: "unconfirmed" } });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByText(/Plugin decision saved/)).toBeNull();
});
