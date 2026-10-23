import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { LocaleProvider } from "./i18n";
import { presentTool, ToolCard, type ToolView } from "./ToolCard";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(cleanup);

function receiptTool(): ToolView {
  return {
    key: "question-call", toolName: "request_user_input", status: "completed",
    text: '{"decision":"submitted","answers":[{"question_id":"scope","value":"private answer"}]}',
    input: '{"questions":[{"id":"scope","question":"Keep this exact question"}]}',
    userInputReceipt: { decision: "submitted", answeredCount: 1 },
    artifactRef: `ta1_${"a".repeat(32)}`, artifactAvailability: "available", artifactPersistedBytes: 100,
  };
}

it("shows a typed question receipt without automatically reading its short JSON artifact", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async () => { throw new Error("explicit read failure"); });
  const { container } = render(<LocaleProvider><ToolCard tool={receiptTool()} onReadArtifact={read} /></LocaleProvider>);
  expect(screen.getByText("Answered 1 question(s).")).toBeTruthy();
  expect(read).not.toHaveBeenCalled();
  expect(container.querySelector(".tool-card-main")?.textContent).not.toContain("private answer");
  const details = screen.getByText("Original request and receipt").closest("details")!;
  expect(details.open).toBe(false);
  expect(details.textContent).toContain("private answer");
  expect(details.textContent).toContain("Keep this exact question");
  await user.click(screen.getByText("Original request and receipt"));
  expect(details.open).toBe(true);
  expect(read).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect(read).toHaveBeenCalledExactlyOnceWith({ kind: "byte_slice", offset: 0, limit: 16_384 });
  expect(await screen.findByRole("alert")).toBeTruthy();
});

it.each([
  ["submitted", "已回答 1 个问题。"],
  ["declined", "已选择暂不回答这些问题。"],
  ["run_cancelled", "已取消本次运行。"],
] as const)("localizes a typed %s receipt without losing its decision", (decision, summary) => {
  window.localStorage.setItem("sigil.desktop.locale.v1", "zh-CN");
  render(<LocaleProvider><ToolCard tool={{ ...receiptTool(), userInputReceipt: { decision, answeredCount: 1 } }} /></LocaleProvider>);
  expect(screen.getByText("询问问题")).toBeTruthy();
  expect(screen.getByText(summary)).toBeTruthy();
  expect(screen.getByText("原始请求与回执").closest("details")?.open).toBe(false);
});

it("preserves errors, legacy JSON and unrelated tools instead of creating an answered claim", () => {
  const tool = receiptTool();
  expect(presentTool({ ...tool, status: "failed", text: "failure detail" })).toMatchObject({
    tone: "danger", summary: "failure detail",
  });
  expect(presentTool({ ...tool, userInputReceipt: undefined }).summary).not.toContain("Answered");
  expect(presentTool({ ...tool, toolName: "custom_question_tool" }).summary).not.toContain("Answered");
});

it("localizes stable names and closed statuses while preserving unknown identifiers and status copy", () => {
  window.localStorage.setItem("sigil.desktop.locale.v1", "zh-CN");
  const { rerender } = render(<LocaleProvider><ToolCard tool={{ key: "read", toolName: "read_file", status: "requested", text: "" }} /></LocaleProvider>);
  expect(screen.getByText("读取文件")).toBeTruthy();
  expect(screen.getByText("已请求")).toBeTruthy();
  rerender(<LocaleProvider><ToolCard tool={{ key: "unknown", toolName: "custom_completed_error_tool", status: "provider_pending_success", text: "" }} /></LocaleProvider>);
  expect(screen.getByText("custom_completed_error_tool")).toBeTruthy();
  expect(screen.getByText("provider_pending_success")).toBeTruthy();
  expect(presentTool({ key: "prototype", toolName: "constructor", status: "toString", text: "" })).toMatchObject({
    displayName: "constructor", status: "toString", tone: "neutral",
  });
});
