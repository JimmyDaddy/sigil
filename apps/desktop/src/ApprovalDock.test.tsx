import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { ApprovalDock } from "./ApprovalDock";
import { LocaleProvider } from "./i18n";
import type { TimelineApproval } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it("offers settings and diagnostics recovery for unsupported shell syntax", () => {
  const onOpenSettings = vi.fn();
  const onOpenSupport = vi.fn();
  const approval: TimelineApproval = {
    callId: "call-shell",
    toolName: "bash",
    approvalRequestId: "approval-shell",
    toolCallHash: "a".repeat(64),
    policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000,
    analysisStatus: "unsupported",
    analysisReasonCodes: ["unsupported_syntax"],
    analysisReasons: ["PowerShell syntax is not supported by the active analyzer"],
    snapshotRequired: false,
  };

  render(
    <LocaleProvider>
      <ApprovalDock
        approval={approval}
        phase="pending"
        busy={false}
        composerRef={createRef<HTMLTextAreaElement>()}
        onDecision={() => undefined}
        onOpenSettings={onOpenSettings}
        onOpenSupport={onOpenSupport}
      />
    </LocaleProvider>,
  );

  expect(screen.getByText("This shell dialect cannot be analyzed safely")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open settings" }));
  fireEvent.click(screen.getByRole("button", { name: "Open support and diagnostics" }));
  expect(onOpenSettings).toHaveBeenCalledOnce();
  expect(onOpenSupport).toHaveBeenCalledOnce();
});

it("keeps an approval actionable when its wire expiry exceeds the JavaScript date range", () => {
  const approval: TimelineApproval = {
    callId: "call-long-lived",
    toolName: "bash",
    approvalRequestId: "approval-long-lived",
    toolCallHash: "b".repeat(64),
    policyVersion: "policy-v2",
    expiresAtMs: Number.MAX_SAFE_INTEGER,
    snapshotRequired: false,
  };

  render(
    <LocaleProvider>
      <ApprovalDock
        approval={approval}
        phase="pending"
        busy={false}
        composerRef={createRef<HTMLTextAreaElement>()}
        onDecision={() => undefined}
        onOpenSettings={() => undefined}
        onOpenSupport={() => undefined}
      />
    </LocaleProvider>,
  );

  expect(screen.getAllByText("not described")).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Approve once" })).toBeTruthy();
});
