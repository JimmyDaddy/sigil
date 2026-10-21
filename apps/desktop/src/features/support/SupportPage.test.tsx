import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { SupportDoctorReport } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { SupportPage } from "./SupportPage";

afterEach(cleanup);

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
