import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

it("keeps the previous diagnosis visible during a recheck and preserves it after a failed refresh", async () => {
  let finish!: (value: SupportDoctorReport) => void;
  const supportDoctor = vi.fn().mockResolvedValueOnce(report)
    .mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }))
    .mockRejectedValueOnce(new Error("unavailable"));
  const { container } = render(<LocaleProvider><NotificationProvider><SupportPage bridge={{ supportDoctor } as unknown as DesktopBridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider></LocaleProvider>);
  await screen.findByText("First independent error.");
  const originalTimestamp = container.querySelector(".support-report-time")?.textContent;
  const details = screen.getByText("First independent error.").closest("details")!;
  expect(details.open).toBe(false);
  fireEvent.click(screen.getByText("Technical details", { selector: "summary span" }));
  expect(details.open).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  expect(screen.getByText("First independent error.")).toBeTruthy();
  expect(details.open).toBe(true);
  expect(container.querySelector(".support-report-time")?.textContent).toBe(originalTimestamp);
  expect((screen.getByRole("button", { name: "Checking Sigil…" }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => finish({ ...report, generatedAtUnixMs: report.generatedAtUnixMs + 60_000,
    checks: [{ status: "warn", name: "Fresh check", summary: "Fresh result." }] }));
  expect(await screen.findByText("Fresh result.")).toBeTruthy();
  const refreshedTimestamp = container.querySelector(".support-report-time")?.textContent;
  expect(refreshedTimestamp).not.toBe(originalTimestamp);
  expect(details.open).toBe(true);
  expect(screen.queryByText("First independent error.")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("previous result"));
  expect(screen.getByText("Fresh result.")).toBeTruthy();
  expect(container.querySelector(".support-report-time")?.textContent).toBe(refreshedTimestamp);
  expect((screen.getByRole("button", { name: "Check again" }) as HTMLButtonElement).disabled).toBe(false);
  expect(supportDoctor).toHaveBeenCalledTimes(3);
});

it("keeps diagnostic statuses in neutral technical details without changing raw records or export privacy", async () => {
  const exportSupportBundle = vi.fn().mockResolvedValue({ cancelled: false, fileName: "private-report.json" });
  const bridge = { supportDoctor: vi.fn().mockResolvedValue(report), exportSupportBundle } as unknown as DesktopBridge;
  const { container } = render(<NotificationProvider><SupportPage bridge={bridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider>);
  await screen.findByRole("heading", { name: "Diagnostic report" });
  const details = container.querySelector(".support-technical-details")! as HTMLDetailsElement;
  expect(details.open).toBe(false);
  expect(container.querySelectorAll(".support-error, .support-warn, .support-ok")).toHaveLength(0);
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("heading", { name: "Errors found" })).toBeNull();
  expect(screen.queryByRole("heading", { name: "Healthy" })).toBeNull();
  expect(container.querySelector(".support-summary")?.textContent).not.toContain("First error");
  const observations = details.querySelector(".support-check-list")! as HTMLElement;
  expect(Array.from(observations.querySelectorAll("strong"), (row) => row.textContent)).toEqual(["Healthy check", "Unknown observation", "First error", "Second error"]);
  expect(Array.from(observations.querySelectorAll("[aria-label='Diagnostic status']"), (row) => row.textContent)).toEqual(["ok", "warn", "error", "error"]);
  expect(screen.getByText("Unknown: probe not available.")).toBeTruthy();
  expect(screen.getByText("Follow the first next step.")).toBeTruthy();
  expect(screen.getByText("This independent fact passed.").closest("details")).toBe(details);
  fireEvent.click(screen.getByText("Technical details", { selector: "summary span" }));
  expect(details.open).toBe(true);
  const authority = screen.getByText("missing_readiness_probe · desktop").closest("details")!;
  expect(authority).toBe(details);
  expect(authority.textContent).toContain("unavailable");
  expect(container.querySelector(".support-summary")!.compareDocumentPosition(container.querySelector(".support-recovery")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect((container.querySelector(".support-recovery") as HTMLDetailsElement).open).toBe(false);
  fireEvent.click(screen.getByText("Command history recovery", { selector: "summary span" }));
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
  await screen.findByRole("heading", { name: locale === "en" ? "Diagnostic report" : "诊断报告" });
  expect(container.querySelector(".application-page-intro")).toBeNull();
  expect(Array.from(container.querySelectorAll(".support-counts dd"), (item) => item.textContent)).toEqual(["42", "4", "3"]);
  const categories = container.querySelector(".support-privacy-columns")! as HTMLElement;
  for (const label of locale === "en" ? ["Build information", "MCP aliases", "Conversation content", "Credential and environment names and values"] : ["版本与构建信息", "MCP 别名", "会话内容", "凭据与环境变量的名称和值"]) expect(within(categories).getByText(label)).toBeTruthy();
  expect(within(categories).getByText("future_included")).toBeTruthy();
  expect(within(categories).getByText("future_excluded")).toBeTruthy();
  const details = container.querySelector(".support-privacy details")! as HTMLDetailsElement;
  expect(details.open).toBe(false);
  expect(within(details).getByText(locale === "en"
    ? "The native shell saves the report you explicitly request; the web renderer never receives its content or destination path."
    : "只有你明确请求时，原生层才会保存报告；Web 渲染层不会收到报告内容或目标路径。")).toBeTruthy();
  expect(details.textContent).toContain("credential_and_environment_names_and_values");
  expect(details.textContent).toContain("future_included");
  expect(container.querySelector(".support-review-note")?.textContent).toContain(locale === "en" ? "Review the saved JSON" : "请先检查保存的 JSON");
  expect(container.querySelector(".support-review-note")?.closest("details")).toBeNull();
  fireEvent.click(screen.getByText(locale === "en" ? "Report contents" : "报告内容", { selector: "summary span" }));
  expect(details.open).toBe(true);
  const technical = container.querySelector(".support-technical-details")! as HTMLDetailsElement;
  expect(technical.open).toBe(false);
  expect(technical.textContent).toContain("Sigil test · test-target");
  expect(technical.textContent).toContain("Unknown: probe not available.");
  expect(screen.queryByRole("heading", { name: locale === "en" ? "Healthy" : "运行正常" })).toBeNull();
});

it("keeps the current diagnosis request failure actionable and allows a direct retry", async () => {
  const supportDoctor = vi.fn().mockRejectedValueOnce(new Error("unavailable")).mockResolvedValueOnce(report);
  render(<NotificationProvider><SupportPage bridge={{ supportDoctor } as unknown as DesktopBridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider>);
  const unavailable = await screen.findByRole("alert");
  expect(unavailable.textContent).toContain("Diagnostics are unavailable");
  fireEvent.click(within(unavailable).getByRole("button", { name: "Retry" }));
  await screen.findByRole("heading", { name: "Diagnostic report" });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(supportDoctor).toHaveBeenCalledTimes(2);
});

it("does not infer repairs or merge independent observations with identical technical text", async () => {
  const sameText = "Review Appearance settings in /config";
  const actual: SupportDoctorReport = { ...report, checks: [
    { status: "warn", name: "appearance:contrast", summary: sameText, remediation: sameText },
    { status: "error", name: "appearance:contrast", summary: sameText, remediation: sameText },
    { status: "warn", name: "future_unknown", summary: "Unverified", remediation: "Repair provider" },
  ] };
  const recoverControlLog = vi.fn();
  const { container } = render(<NotificationProvider><SupportPage bridge={{ supportDoctor: async () => actual, recoverControlLog } as unknown as DesktopBridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider>);
  await screen.findByRole("heading", { name: "Diagnostic report" });
  const details = container.querySelector(".support-technical-details")! as HTMLDetailsElement;
  expect(details.open).toBe(false);
  expect(details.querySelectorAll(".support-check")).toHaveLength(3);
  expect(screen.getAllByText("appearance:contrast")).toHaveLength(2);
  expect(screen.queryByRole("button", { name: "Repair provider" })).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(recoverControlLog).not.toHaveBeenCalled();
  expect(actual.checks.map((check) => check.status)).toEqual(["warn", "error", "warn"]);
});

it.each(["failed", "cancelled"] as const)("preserves the report when native export is %s", async (outcome) => {
  const exportSupportBundle = outcome === "failed"
    ? vi.fn().mockRejectedValue(new Error("save unavailable"))
    : vi.fn().mockResolvedValue({ cancelled: true });
  render(<NotificationProvider><SupportPage bridge={{ supportDoctor: async () => report, exportSupportBundle } as unknown as DesktopBridge} workspaceId="workspace" onBack={vi.fn()} /></NotificationProvider>);
  await screen.findByRole("heading", { name: "Diagnostic report" });
  fireEvent.click(screen.getByRole("button", { name: "Save private report" }));
  if (outcome === "failed") {
    await screen.findByText("Support report not saved");
    expect(screen.getByText("Choose another local destination and try again.")).toBeTruthy();
  }
  await waitFor(() => expect((screen.getByRole("button", { name: "Save private report" }) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.getByText("First independent error.")).toBeTruthy();
  expect(screen.queryByText("Support report saved")).toBeNull();
  expect(exportSupportBundle).toHaveBeenCalledWith("workspace", []);
});
