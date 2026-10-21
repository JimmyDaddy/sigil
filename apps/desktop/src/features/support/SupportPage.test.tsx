import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { SupportDoctorReport } from "../../types";
import { LocaleProvider } from "../../i18n";
import { NotificationProvider } from "../../ui/feedback";
import { SupportPage } from "./SupportPage";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const report: SupportDoctorReport = {
  generatedAtUnixMs: 1_784_419_200_000, version: "test", commit: "test", target: "test-target", profile: "debug",
  environment: { os: "test", architecture: "test", terminalFamily: "other" },
  cutover: { schemaVersion: 1, epoch: "unavailable", authority: "unavailable", blockers: [{ code: "missing_readiness_probe", adapter: "desktop" }] },
  summary: { overallStatus: "error", ok: 1, warn: 1, error: 2 },
  checks: [
    { status: "ok", name: "Healthy check", summary: "This independent fact passed." },
    { status: "warn", name: "Unknown observation", summary: "Unknown: probe not available.", remediation: "Inspect this observation." },
    { status: "error", name: "First error", summary: "First independent error.", remediation: "Follow the first next step." },
    { status: "error", name: "Second error", summary: "Second independent error." },
  ],
  privacy: { included: ["redacted build metadata"], excluded: ["credentials", "conversation content"], reviewBeforeSharing: true },
};

it("prioritizes errors and warnings before optional healthy and recovery facts without changing export privacy", async () => {
  const exportSupportBundle = vi.fn().mockResolvedValue({ cancelled: false, fileName: "private-report.json" });
  const bridge = { supportDoctor: vi.fn().mockResolvedValue(report), exportSupportBundle } as unknown as DesktopBridge;
  const { container } = render(<NotificationProvider><SupportPage bridge={bridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider>);
  await screen.findByRole("heading", { name: "Errors found" });
  const attention = container.querySelector(".support-check-list")! as HTMLElement;
  expect(within(attention).getAllByRole("listitem").map((row) => row.querySelector("strong")?.textContent)).toEqual(["First error", "Second error", "Unknown observation"]);
  expect(screen.getByText("Unknown: probe not available.")).toBeTruthy();
  expect(screen.getByText("Follow the first next step.")).toBeTruthy();
  const healthy = screen.getByText("This independent fact passed.").closest("details")!;
  expect(healthy.open).toBe(false);
  fireEvent.click(screen.getByText("Passed checks (1)"));
  expect(healthy.open).toBe(true);
  const authority = screen.getByText("missing_readiness_probe · desktop").closest("details")!;
  expect(authority.open).toBe(false);
  fireEvent.click(screen.getByText("Resource authority", { selector: "summary span" }));
  expect(authority.open).toBe(true);
  expect(authority.textContent).toContain("unavailable");
  expect(container.querySelector(".support-summary")!.compareDocumentPosition(container.querySelector(".support-recovery")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect((container.querySelector(".support-recovery") as HTMLDetailsElement).open).toBe(false);
  expect((screen.getByRole("button", { name: "Preview recovery" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText("credentials")).toBeTruthy();
  expect(screen.getByText("conversation content")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Save private report" }));
  await screen.findByText("Support report saved");
  expect(exportSupportBundle).toHaveBeenCalledWith("workspace", []);
});

it("does not replace the new workspace diagnosis with a late previous workspace result", async () => {
  let resolveOld!: (report: SupportDoctorReport) => void;
  const supportDoctor = vi.fn()
    .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
    .mockResolvedValueOnce({ ...report, checks: [{ status: "warn", name: "Current workspace", summary: "Current fact." }] });
  const bridge = { supportDoctor } as unknown as DesktopBridge;
  const page = (workspaceId: string) => <NotificationProvider><SupportPage bridge={bridge} workspaceId={workspaceId} onBack={vi.fn()} /></NotificationProvider>;
  const { rerender } = render(page("old"));
  rerender(page("new"));
  await screen.findByText("Current fact.");
  await act(async () => resolveOld(report));
  expect(screen.getByText("Current fact.")).toBeTruthy();
  expect(screen.queryByText("First independent error.")).toBeNull();
});

it.each(["en", "zh-CN"] as const)("preserves compact counts and localizes known privacy IDs in %s while retaining unknown categories and exact identifiers", async (locale) => {
  window.localStorage.setItem("sigil.desktop.locale.v1", locale);
  const actual = { ...report, summary: { ...report.summary, ok: 42, warn: 4, error: 3 },
    privacy: { included: ["build_metadata", "mcp_aliases", "future_included"], excluded: ["conversation_content", "credential_and_environment_names_and_values", "future_excluded"], reviewBeforeSharing: true } };
  const bridge = { supportDoctor: vi.fn().mockResolvedValue(actual) } as unknown as DesktopBridge;
  const { container } = render(<LocaleProvider><NotificationProvider><SupportPage bridge={bridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider></LocaleProvider>);
  await screen.findByRole("heading", { name: locale === "en" ? "Errors found" : "发现错误" });
  expect(container.querySelector(".application-page-intro")).toBeNull();
  expect(Array.from(container.querySelectorAll(".support-counts dd"), (item) => item.textContent)).toEqual(["42", "4", "3"]);
  const categories = container.querySelector(".support-privacy-columns")! as HTMLElement;
  for (const label of locale === "en" ? ["Build information", "MCP aliases", "Conversation content", "Credential and environment names and values"] : ["版本与构建信息", "MCP 别名", "会话内容", "凭据与环境变量的名称和值"]) expect(within(categories).getByText(label)).toBeTruthy();
  expect(within(categories).getByText("future_included")).toBeTruthy();
  expect(within(categories).getByText("future_excluded")).toBeTruthy();
  const details = container.querySelector(".support-privacy details")! as HTMLDetailsElement;
  expect(details.open).toBe(false);
  expect(details.textContent).toContain("credential_and_environment_names_and_values");
  expect(details.textContent).toContain("future_included");
  expect(container.querySelector(".support-review-note")?.textContent).toContain(locale === "en" ? "Review the saved JSON" : "请先检查保存的 JSON");
  const authority = container.querySelector(".support-checks.sg-collapsible")!;
  expect(authority.textContent).toContain("Sigil test · test-target");
});
