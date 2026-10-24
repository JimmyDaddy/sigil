import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createRef } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { ApprovalDock } from "./ApprovalDock";
import { LocaleProvider } from "./i18n";
import type { TimelineApproval } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it.each(["", " \n "])("shows the actual impact when the preview has no summary (%j)", (previewSummary) => {
  const boundary = "External directory access for exec_command\n/approved external/project\n\n";
  const context = "Working directory (relative to workspace unless absolute): /approved external/project\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: 7 seconds";
  const command = "printf 'exact action'  \n";
  const impact = "This command requires destructive confirmation; the external target remains constrained.";
  const approval: TimelineApproval = { callId: "empty-summary", toolName: "exec_command",
    approvalRequestId: "empty-summary-request", toolCallHash: "b".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false,
    operation: "execute_destructive_command", risk: "destructive", analysisStatus: "conservative",
    previewSummary, safeSummaryDetail: impact, previewBody: `${boundary}${context}\n\nCommand:\n${command}` };
  const onDecision = vi.fn();
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} /></LocaleProvider>);
  expect(screen.getByText(impact).closest("details")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Complete command" }).textContent).toBe(command);
  expect(document.querySelector(".approval-external-boundary")?.textContent).toBe(boundary);
  expect(screen.getByText("Destructive").closest("details")).toBeNull();
  expect(screen.getByText("Some effects could not be identified. Check the exact action and its targets before approving.")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(false);
  expect(onDecision).not.toHaveBeenCalled();
});

it("preserves a nonempty unknown summary and uses localized guidance only when both summaries are empty", () => {
  const note = "  Additional service-specific impact must remain visible.  ";
  const approval: TimelineApproval = { callId: "unknown-summary", toolName: "custom_tool",
    approvalRequestId: "unknown-summary-request", toolCallHash: "c".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false,
    previewSummary: note, safeSummaryDetail: "A different structured impact", previewBody: "Exact custom action" };
  const props = { phase: "pending" as const, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision: vi.fn(), onOpenSettings: vi.fn(), onOpenSupport: vi.fn() };
  const view = render(<LocaleProvider><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  expect(review.querySelector("p")?.textContent).toBe(note);
  expect(review.querySelector("p")?.closest("details")).toBeNull();
  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval,
    previewSummary: "", safeSummaryDetail: " \n " }} /></LocaleProvider>);
  expect(screen.getByText("Review this exact tool action before the run can continue.").closest("details")).toBeNull();
  expect(props.onDecision).not.toHaveBeenCalled();
});

it("localizes destructive file facts while keeping exact impact and snapshot binding available", () => {
  window.localStorage.setItem("sigil.desktop.locale.v1", "zh-CN");
  const onDecision = vi.fn();
  const impact = "This replaces the exact existing file; external access remains constrained.";
  const approval: TimelineApproval = { callId: "file-facts", approvalRequestId: "file-request", toolName: "write_file",
    toolCallHash: "a".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
    risk: "destructive", operation: "overwrite_file", effects: ["file_write"], subjects: ["README.md"],
    previewBody: "--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n",
    previewSummary: impact, snapshotRequired: true,
    sessionGrantAvailable: false, sessionGrantUnavailableReason: { code: "snapshot_required" } };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} /></LocaleProvider>);
  expect(screen.getByRole("heading", { name: "覆盖文件" })).toBeTruthy();
  expect(screen.getByText("破坏性操作").closest("details")).toBeNull();
  expect(screen.getByText(impact).closest("details")).toBeNull();
  const mechanism = screen.getByText("当前决定与文件快照绑定，因此不能在本会话持续授权。");
  expect(mechanism.closest("details")?.open).toBe(false);
  expect(screen.queryByRole("button", { name: "本会话允许" })).toBeNull();
  expect(onDecision).not.toHaveBeenCalled();
});

it("uses a compact acceptance receipt while keeping uncertain delivery actionable", () => {
  const approval: TimelineApproval = { callId: "receipt-call", toolName: "exec_command",
    approvalRequestId: "receipt-request", toolCallHash: "a".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false };
  const props = { approval, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision: vi.fn(), onOpenSettings: vi.fn(), onOpenSupport: vi.fn() };
  const view = render(<LocaleProvider><ApprovalDock {...props} phase="accepted" /></LocaleProvider>);
  expect(screen.getByRole("status").classList.contains("approval-receipt")).toBe(true);
  expect(view.container.querySelector(".approval-dock")).toBeNull();
  expect(view.container.querySelector("details")?.open).toBe(false);
  expect(screen.queryByRole("button", { name: "Approve once" })).toBeNull();
  view.rerender(<LocaleProvider><ApprovalDock {...props} phase="delivery_uncertain" /></LocaleProvider>);
  expect(view.container.querySelector(".approval-dock-delivery_uncertain")).toBeTruthy();
  expect(screen.getByText("Approval status needs confirmation")).toBeTruthy();
  expect(view.container.querySelector(".approval-receipt")).toBeNull();
  expect(props.onDecision).not.toHaveBeenCalled();
});

it("focuses the review scroll region for a new exact request without stealing same-guard focus", () => {
  const composerRef = createRef<HTMLTextAreaElement>();
  const onDecision = vi.fn();
  const approval: TimelineApproval = { callId: "focus-call", toolName: "exec_command",
    approvalRequestId: "focus-request", toolCallHash: "a".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false, operation: "execute_unknown_command",
    previewTitle: "Read the exact command", previewBody: "Command:\nprintf 'review only'" };
  const view = (request: TimelineApproval) => <LocaleProvider><textarea ref={composerRef} />
    <ApprovalDock approval={request} phase="pending" busy={false} composerRef={composerRef}
      onDecision={onDecision} onOpenSettings={() => undefined} onOpenSupport={() => undefined} /></LocaleProvider>;
  const rendered = render(view(approval));
  const review = screen.getByRole("region", { name: "Requested action" });
  expect(document.activeElement).toBe(review);
  const deny = screen.getByRole("button", { name: "Deny" });
  deny.focus();
  rendered.rerender(view({ ...approval, previewBody: `${approval.previewBody}\n# unchanged exact request` }));
  expect(document.activeElement).toBe(deny);
  rendered.rerender(view({ ...approval, approvalRequestId: "next-focus-request", toolCallHash: "b".repeat(64) }));
  expect(document.activeElement).toBe(review);
  fireEvent.keyDown(review, { key: "Escape" });
  expect(document.activeElement).toBe(composerRef.current);
  expect(onDecision).not.toHaveBeenCalled();
});

it("keeps missing-preview recovery visible and never grants or steals focus when the same preview returns", () => {
  const composerRef = createRef<HTMLTextAreaElement>();
  const onDecision = vi.fn();
  const approval: TimelineApproval = { callId: "missing-focus-call", toolName: "write_file",
    approvalRequestId: "missing-focus-request", toolCallHash: "c".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: true, operation: "edit_file" };
  const view = (request: TimelineApproval) => <LocaleProvider><textarea ref={composerRef} />
    <ApprovalDock approval={request} phase="pending" busy={false} composerRef={composerRef}
      onDecision={onDecision} onOpenSettings={() => undefined} onOpenSupport={() => undefined} /></LocaleProvider>;
  const rendered = render(view(approval));
  expect(document.activeElement?.classList.contains("approval-dock")).toBe(true);
  expect(screen.getByRole("alert").textContent).toContain("The exact file change is unavailable");
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(true);
  const deny = screen.getByRole("button", { name: "Deny" });
  deny.focus();
  rendered.rerender(view({ ...approval, previewBody: "- before\n+ after" }));
  expect(document.activeElement).toBe(deny);
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(false);
  expect(onDecision).not.toHaveBeenCalled();
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

  expect(screen.getByText("This command needs manual review")).toBeTruthy();
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

it.each([
  { effects: ["file_write"] },
  { effects: ["file_delete"] },
  { operation: "overwrite_file" },
  { operation: "apply_change_set" },
])("blocks all file mutation grants without the exact review: %j", (fileAction) => {
  const onDecision = vi.fn();
  const onOpenSupport = vi.fn();
  const approval: TimelineApproval = {
    callId: "call-file", toolName: "arbitrary-tool", approvalRequestId: "file-request",
    toolCallHash: "c".repeat(64), policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000, snapshotRequired: true,
    sessionGrantAvailable: true, commandFamilyAllowPattern: "tool:*", ...fileAction,
  };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={() => undefined} onOpenSupport={onOpenSupport} /></LocaleProvider>);

  expect(screen.getByRole("alert").textContent).toContain("The exact file change is unavailable");
  for (const button of screen.getAllByRole("button").filter((button) => button.textContent?.startsWith("Approve"))) {
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(button);
  }
  expect(onDecision).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Open support and diagnostics" }));
  expect(onOpenSupport).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect(onDecision).toHaveBeenCalledWith("deny");
});

it("enables the exact file request when canonical review text returns", () => {
  const onDecision = vi.fn();
  const approval: TimelineApproval = {
    callId: "call-file", toolName: "write_file", approvalRequestId: "file-request",
    toolCallHash: "c".repeat(64), policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000, snapshotRequired: true,
    effects: ["file_write"], previewBody: "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
  };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={() => undefined} onOpenSupport={() => undefined} /></LocaleProvider>);
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Approve once" }));
  expect(onDecision).toHaveBeenCalledWith("approve_once");
});

it.each([
  { effects: ["file_write", "execute_dynamic_code", "network_unknown", "remote_mutation", "unknown"] },
  { effects: ["network_unknown", "remote_mutation", "unknown"] },
])("reviews opaque network requests without inventing a file change or reusable grant: %j", ({ effects }) => {
  const onDecision = vi.fn();
  const approval: TimelineApproval = {
    callId: "opaque-call", toolName: "arbitrary-service-tool", approvalRequestId: "opaque-request",
    toolCallHash: "e".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
    operation: "network_request", risk: "high", snapshotRequired: false,
    effects,
    analysisStatus: "conservative", safeSummaryDetail: "The service's effects are not fully known.",
    sessionGrantAvailable: false, sessionGrantUnavailableReason: { code: "analysis_incomplete" },
    previewBody: '{"server":"fixture","tool":"echo","arguments":{"text":"中文"}}',
  };
  const props = { busy: false, phase: "pending" as const, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision, onOpenSettings: vi.fn(), onOpenSupport: vi.fn() };
  const view = render(<LocaleProvider><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  expect(screen.getByText("High risk")).toBeTruthy();
  expect(screen.getByRole("region", { name: "Requested action" }).textContent).toContain('"text":"中文"');
  expect(screen.queryByText("The exact file change is unavailable")).toBeNull();
  expect(screen.queryByRole("button", { name: "Allow for session" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Allow family" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Approve once" }));
  expect(onDecision).toHaveBeenCalledOnce();
  expect(onDecision).toHaveBeenCalledWith("approve_once");
  onDecision.mockClear();

  // Raw arguments from a different timeline projection are not frozen review evidence.
  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval,
    approvalRequestId: "next-opaque-request", toolCallHash: "f".repeat(64),
    previewBody: undefined, toolInput: "RAW INPUT MUST NOT AUTHORIZE" }} /></LocaleProvider>);
  expect(screen.getByRole("alert").textContent).toContain("The exact action is unavailable");
  expect(screen.queryByText("The exact file change is unavailable")).toBeNull();
  const approve = screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement;
  expect(approve.disabled).toBe(true);
  fireEvent.click(approve);
  expect(onDecision).not.toHaveBeenCalled();

  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval, expiresAtMs: 1 }} /></LocaleProvider>);
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(true);
});

it("preserves one-time approval for a lifecycle network request without a mutation preview", () => {
  const onDecision = vi.fn();
  render(<LocaleProvider><ApprovalDock phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} approval={{
      callId: "lifecycle-call", toolName: "arbitrary-lifecycle-action", approvalRequestId: "lifecycle-request",
      toolCallHash: "a".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
      operation: "network_request", effects: ["network_unknown", "unknown"],
      analysisStatus: "complete", risk: "high", snapshotRequired: false,
      safeSummaryDetail: "Activate the configured service.", sessionGrantAvailable: false,
      sessionGrantUnavailableReason: { code: "non_grantable_effect" },
    }} /></LocaleProvider>);
  expect(screen.getByText("High risk")).toBeTruthy();
  expect(screen.getByText("Activate the configured service.")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("button", { name: "Allow for session" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Allow family" })).toBeNull();
  const approve = screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement;
  expect(approve.disabled).toBe(false);
  expect(onDecision).not.toHaveBeenCalled();
  fireEvent.click(approve);
  expect(onDecision).toHaveBeenCalledOnce();
  expect(onDecision).toHaveBeenCalledWith("approve_once");
});

it.each([
  { operation: undefined },
  { operation: "unknown_operation" },
  { operation: "edit_file" },
])("keeps unknown and explicit file mutations guarded with opaque effects: %j", (operation) => {
  const onDecision = vi.fn();
  render(<LocaleProvider><ApprovalDock phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={onDecision}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} approval={{
      callId: "file-fallback", toolName: "arbitrary-tool", approvalRequestId: "file-fallback-request",
      toolCallHash: "e".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
      effects: ["file_write", "unknown"], snapshotRequired: true, ...operation,
    }} /></LocaleProvider>);
  expect(screen.getByRole("alert").textContent).toContain("The exact file change is unavailable");
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(true);
  expect(onDecision).not.toHaveBeenCalled();
});

it("shows one frozen command review before and after canonical reconciliation", () => {
  const command = "python3 split_trip.py bill.csv";
  const body = `Working directory: .\nShell: sh (posix)\n\nCommand:\n${command}`;
  const approval: TimelineApproval = {
    callId: "call-single-review", toolName: "exec_command", approvalRequestId: "single-review",
    toolCallHash: "e".repeat(64), policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000, snapshotRequired: false,
    operation: "execute_unknown_command", toolInput: command, previewBody: body,
  };
  const props = { phase: "pending" as const, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision: vi.fn(), onOpenSettings: () => undefined, onOpenSupport: () => undefined };
  const view = render(<LocaleProvider><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  const frozenBody = review.querySelector("pre");
  expect(review.querySelectorAll("pre")).toHaveLength(1);
  expect(frozenBody?.textContent).toBe(body);
  expect(screen.queryByText(command, { exact: true })).toBeNull();
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(false);

  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval, toolInput: undefined }} /></LocaleProvider>);
  expect(review.querySelector("pre")).toBe(frozenBody);
  expect(frozenBody?.textContent).toBe(body);
  expect(review.querySelectorAll("pre")).toHaveLength(1);

  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval, previewBody: undefined }} /></LocaleProvider>);
  expect(review.querySelector("pre")?.textContent).toBe(command);
  expect(screen.getByRole("alert").textContent).toContain("The complete command is unavailable");
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(true);
});

it("puts the complete frozen command first and keeps its exact environment expandable across canonical updates", () => {
  const command = "  python3 -m unittest -v test_agenda.py\n" + "# 保留全部原文与尾空格  \n".repeat(90) + "printf 'C7 EXACT TAIL'  \n";
  const context = "Working directory (relative to workspace unless absolute): .\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: no explicit deadline";
  const approval: TimelineApproval = {
    callId: "c7-command", toolName: "exec_command", approvalRequestId: "c7-request",
    toolCallHash: "e".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
    snapshotRequired: false, operation: "execute_unknown_command", risk: "high",
    analysisStatus: "conservative", previewTitle: "Execute a shell command",
    previewSummary: "Review the complete command and its execution context.",
    previewBody: `${context}\n\nCommand:\n${command}`, toolInput: "UNTRUSTED RAW INPUT",
  };
  const onDecision = vi.fn();
  const props = { phase: "pending" as const, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision, onOpenSettings: vi.fn(), onOpenSupport: vi.fn() };
  const rendered = render(<LocaleProvider><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  const exactCommand = screen.getByRole("textbox", { name: "Complete command" });
  expect(review.firstElementChild).toBe(exactCommand);
  expect(exactCommand.textContent).toBe(command);
  expect(exactCommand.getAttribute("aria-readonly")).toBe("true");
  expect(exactCommand.getAttribute("aria-multiline")).toBe("true");
  expect(exactCommand.querySelector("code")?.textContent).toBe(command);
  expect(document.activeElement).toBe(review);
  expect(screen.getByRole("heading", { name: "Run command" })).toBeTruthy();
  expect(review.querySelector("p")?.textContent).toBe(approval.previewSummary);
  const environment = screen.getByText("Execution environment").closest("details")!;
  expect(environment.open).toBe(false);
  expect(within(environment).getByText(".")).toBeTruthy();
  expect(within(environment).getByText("sh (posix)")).toBeTruthy();
  expect(within(environment).getByText("pipe")).toBeTruthy();
  expect(within(environment).getByText("No explicit limit")).toBeTruthy();
  expect(screen.queryByText("UNTRUSTED RAW INPUT")).toBeNull();
  expect(screen.getByText("Some effects could not be identified. Check the exact action and its targets before approving.")).toBeTruthy();
  fireEvent.click(within(environment).getByText("Execution environment"));
  expect(environment.open).toBe(true);
  const deny = screen.getByRole("button", { name: "Deny" });
  deny.focus();
  rendered.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval, toolInput: undefined }} /></LocaleProvider>);
  expect(screen.getByRole("textbox", { name: "Complete command" })).toBe(exactCommand);
  expect(exactCommand.textContent).toBe(command);
  expect(environment.open).toBe(true);
  expect(document.activeElement).toBe(deny);
  expect(onDecision).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Approve once" }));
  expect(onDecision).toHaveBeenCalledExactlyOnceWith("approve_once");
});

it("keeps frozen external targets visible separately from the command environment", () => {
  const boundary = "External directory access for exec_command\n/approved external/project\n/approved external/second\n\n";
  const context = "Working directory (relative to workspace unless absolute): /approved external/project\nShell: /bin/sh (posix)\nTerminal I/O: pty\nMaximum runtime: 120 seconds";
  const command = "printf 'outside workspace'\n";
  const approval: TimelineApproval = { callId: "external-command", toolName: "exec_command",
    approvalRequestId: "external-request", toolCallHash: "a".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false, operation: "execute_unknown_command",
    risk: "high", previewBody: `${boundary}${context}\n\nCommand:\n${command}` };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={vi.fn()}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  const targets = review.querySelector(".approval-external-boundary")!;
  expect(targets.textContent).toBe(boundary);
  expect(targets.closest("details")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Complete command" }).textContent).toBe(command);
  const environment = screen.getByText("Execution environment").closest("details")!;
  expect(within(environment).getByText("/bin/sh (posix)")).toBeTruthy();
  expect(within(environment).getByText("120 seconds")).toBeTruthy();
});

it.each([
  "Working directory: .\nShell: sh (posix)\n\nCommand:\nprintf legacy",
  "Working directory (relative to workspace unless absolute): .\ninjected\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: no explicit deadline\n\nCommand:\nprintf multiline-path",
  "Working directory (relative to workspace unless absolute): .\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: no explicit deadline\n\nCommand:\nprintf here\nCommand:\nsecond marker",
  "Unknown risk prefix\n\nWorking directory (relative to workspace unless absolute): .\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: no explicit deadline\n\nCommand:\nprintf unknown",
])("retains an unrecognized or ambiguous frozen command in full before secondary explanations", (body) => {
  const approval: TimelineApproval = { callId: "fallback-command", toolName: "exec_command",
    approvalRequestId: "fallback-request", toolCallHash: "a".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false, operation: "execute_unknown_command",
    previewBody: body, previewSummary: "Important unknown risk remains visible", toolInput: "NOT REVIEW EVIDENCE" };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={vi.fn()}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  expect(review.firstElementChild?.tagName).toBe("PRE");
  expect(review.firstElementChild?.textContent).toBe(body);
  expect(screen.queryByRole("textbox", { name: "Complete command" })).toBeNull();
  expect(review.querySelector("details.approval-execution-context")).toBeNull();
  expect(screen.getByText("Important unknown risk remains visible").closest("details")).toBeNull();
  expect(screen.queryByText("NOT REVIEW EVIDENCE")).toBeNull();
});

it("preserves another tool's complete frozen review and title even when its display resembles the built-in format", () => {
  const body = "Working directory (relative to workspace unless absolute): .\nShell: sh (posix)\nTerminal I/O: pipe\nMaximum runtime: no explicit deadline\n\nCommand:\nprintf custom";
  const approval: TimelineApproval = { callId: "custom-command", toolName: "custom_executor",
    approvalRequestId: "custom-request", toolCallHash: "a".repeat(64), policyVersion: "manual",
    expiresAtMs: Number.MAX_SAFE_INTEGER, snapshotRequired: false, operation: "execute_unknown_command",
    previewTitle: "Custom execution boundary", previewBody: body };
  render(<LocaleProvider><ApprovalDock approval={approval} phase="pending" busy={false}
    composerRef={createRef<HTMLTextAreaElement>()} onDecision={vi.fn()}
    onOpenSettings={vi.fn()} onOpenSupport={vi.fn()} /></LocaleProvider>);
  expect(screen.getByRole("heading", { name: "Custom execution boundary" })).toBeTruthy();
  expect(screen.getByRole("region", { name: "Requested action" }).firstElementChild?.textContent).toBe(body);
  expect(screen.queryByRole("textbox", { name: "Complete command" })).toBeNull();
});

it("keeps complete plain and diff reviews in one keyboard-focusable scroll region outside the actions", () => {
  // jsdom parses layers but does not apply their cascade. Reuse the real feature rules
  // without that wrapper; this checks CSS declarations, never fabricated geometry.
  const parsedStyles = document.createElement("style");
  parsedStyles.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
  document.head.append(parsedStyles);
  const features = Array.from(parsedStyles.sheet!.cssRules).find((rule) =>
    "name" in rule && rule.name === "features"
  ) as CSSGroupingRule;
  const reviewStyles = Array.from(features.cssRules, (rule) => rule.cssText).join("\n");
  parsedStyles.remove();
  const lines = Array.from({ length: 400 }, (_, index) => `line ${index}: ${"long ".repeat(30)}`);
  const tail = "exact-review-tail-canary";
  const approval: TimelineApproval = {
    callId: "call-long-review", toolName: "write_file", approvalRequestId: "long-review",
    toolCallHash: "f".repeat(64), policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000, snapshotRequired: true,
    effects: ["file_write"], previewBody: [...lines, tail].join("\n"),
  };
  const props = { phase: "pending" as const, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision: vi.fn(), onOpenSettings: () => undefined, onOpenSupport: () => undefined };
  const view = render(<LocaleProvider><style>{reviewStyles}</style><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  const review = screen.getByRole("region", { name: "Requested action" });
  review.focus();
  expect(document.activeElement).toBe(review);
  expect(getComputedStyle(review).overflowY).toBe("auto");
  expect(getComputedStyle(review).gridAutoRows).toBe("max-content");
  expect(getComputedStyle(review).minHeight).toBe("0px");
  expect(review.contains(screen.getByRole("heading", { level: 3 }))).toBe(false);
  expect(review.contains(screen.getByRole("button", { name: "Approve once" }))).toBe(false);

  for (const body of [approval.previewBody!, `--- a/file\n+++ b/file\n@@ -1 +1 @@\n${lines.map((line) => `+${line}`).join("\n")}\n+${tail}`]) {
    view.rerender(<LocaleProvider><style>{reviewStyles}</style><ApprovalDock {...props} approval={{ ...approval, previewBody: body }} /></LocaleProvider>);
    const pre = review.querySelector("pre")!;
    expect(pre.textContent).toContain(tail);
    expect(getComputedStyle(pre).maxHeight).toBe("none");
    expect(getComputedStyle(pre).overflow).toBe("visible");
    expect(getComputedStyle(pre).whiteSpace).toBe("pre-wrap");
  }
  expect(getComputedStyle(review.querySelector(".diff-viewer")!).overflow).toBe("visible");
  expect(getComputedStyle(review.querySelector(".diff-viewer pre span")!).minWidth).toBe("0px");

  view.rerender(<LocaleProvider><style>{reviewStyles}</style><ApprovalDock {...props} approval={{ ...approval,
    effects: undefined, operation: "execute_unknown_command", previewBody: undefined,
    toolInput: [...lines, tail].join("\n"),
  }} /></LocaleProvider>);
  const raw = review.querySelector(".approval-command pre")!;
  expect(raw.textContent).toContain(tail);
  expect(getComputedStyle(raw).maxHeight).toBe("none");
  expect(getComputedStyle(raw).overflow).toBe("visible");
  expect((screen.getByRole("button", { name: "Approve once" }) as HTMLButtonElement).disabled).toBe(true);
});

it.each([
  "execute_read_only_command", "execute_workspace_check_command", "execute_mutating_command",
  "execute_unknown_command", "execute_destructive_command",
])("requires a complete frozen command for typed operation %s", (operation) => {
  const onDecision = vi.fn();
  const approval: TimelineApproval = {
    callId: "call-command", toolName: "arbitrary-tool", approvalRequestId: "command-request",
    toolCallHash: "d".repeat(64), policyVersion: "policy-v2",
    expiresAtMs: Date.now() + 60_000, snapshotRequired: false, operation,
    toolInput: "untrusted raw argument fallback", sessionGrantAvailable: true,
    commandFamilyAllowPattern: "command:*",
  };
  const props = { phase: "pending" as const, busy: false, composerRef: createRef<HTMLTextAreaElement>(),
    onDecision, onOpenSettings: () => undefined, onOpenSupport: () => undefined };
  const view = render(<LocaleProvider><ApprovalDock {...props} approval={approval} /></LocaleProvider>);
  expect(screen.getByRole("alert").textContent).toContain("The complete command is unavailable");
  for (const name of ["Approve once", "Allow for session", "Allow family"]) {
    const button = screen.getByRole("button", { name }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
  }
  expect(onDecision).not.toHaveBeenCalled();
  expect((screen.getByRole("button", { name: "Deny" }) as HTMLButtonElement).disabled).toBe(false);
  view.rerender(<LocaleProvider><ApprovalDock {...props} approval={{ ...approval,
    previewBody: "Working directory: .\nShell: sh (posix)\n\nCommand:\nprintf checked",
  }} /></LocaleProvider>);
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Approve once" }));
  expect(onDecision).toHaveBeenCalledWith("approve_once");
});
