import { readFileSync } from "node:fs";
import { resolve } from "node:path";
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
