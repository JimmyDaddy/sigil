import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import { McpImportCard } from "./McpImportCard";

afterEach(cleanup);

function fixture(overrides: Partial<DesktopBridge> = {}) {
  const applyMcpImport = vi.fn(async () => ({ importedNames: ["alpha"], reloadRequired: false }));
  const bridge = {
    pickMcpImport: async () => ({ previewId: "preview", rootFieldsIgnored: true, candidates: [
      { index: 0, name: "alpha", transport: "stdio", description: "", importable: true,
        issues: ["environment_values_not_imported", "command_arguments_will_be_saved"] },
      { index: 1, name: "beta", transport: "streamable_http", description: "", importable: true, issues: [] },
      { index: 2, name: "invalid", transport: null, description: "", importable: false, issues: ["invalid_configuration"] },
    ] }),
    applyMcpImport,
    ...overrides,
  } as unknown as DesktopBridge;
  const onImported = vi.fn(async () => undefined);
  render(<LocaleProvider><McpImportCard bridge={bridge} workspaceId="workspace" onImported={onImported} /></LocaleProvider>);
  return { applyMcpImport, onImported };
}

it("previews first, publishes only the selection, and does not need a model credential", async () => {
  const user = userEvent.setup();
  const { applyMcpImport, onImported } = fixture();
  await user.click(screen.getByRole("button", { name: "Choose JSON file" }));
  expect(applyMcpImport).not.toHaveBeenCalled();
  expect((screen.getByRole("checkbox", { name: "invalid" }) as HTMLInputElement).disabled).toBe(true);
  expect(screen.getByText(/Inline environment and header values are not copied/)).toBeTruthy();
  await user.click(screen.getByRole("checkbox", { name: "beta" }));
  await user.click(screen.getByRole("button", { name: "Import selected servers" }));
  expect(applyMcpImport).toHaveBeenCalledWith("workspace", "preview", [0]);
  expect((await screen.findByRole("status")).textContent).toContain("MCP servers imported: alpha");
  expect(onImported).toHaveBeenCalledOnce();
});

it("retains the exact preview and selection after a save failure", async () => {
  const user = userEvent.setup();
  const applyMcpImport = vi.fn().mockRejectedValueOnce(new Error("changed"))
    .mockResolvedValueOnce({ importedNames: ["alpha"], reloadRequired: false });
  fixture({ applyMcpImport });
  await user.click(screen.getByRole("button", { name: "Choose JSON file" }));
  await user.click(screen.getByRole("checkbox", { name: "beta" }));
  await user.click(screen.getByRole("button", { name: "Import selected servers" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Your selection is preserved");
  expect((screen.getByRole("checkbox", { name: "alpha" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "beta" }) as HTMLInputElement).checked).toBe(false);
  await user.click(screen.getByRole("button", { name: "Import selected servers" }));
  expect(applyMcpImport).toHaveBeenNthCalledWith(2, "workspace", "preview", [0]);
});

it("reports a saved import accurately when workspace reload needs recovery", async () => {
  const user = userEvent.setup();
  const { onImported } = fixture({ applyMcpImport: async () => ({ importedNames: ["alpha"], reloadRequired: true }) });
  await user.click(screen.getByRole("button", { name: "Choose JSON file" }));
  await user.click(screen.getByRole("button", { name: "Import selected servers" }));
  expect((await screen.findByRole("status")).textContent).toContain("MCP servers imported");
  expect(screen.getByRole("alert").textContent).toContain("Import was saved");
  expect(onImported).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "Import selected servers" })).toBeNull();
});
