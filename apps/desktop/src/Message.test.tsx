import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { LocaleProvider } from "./i18n";
import { Message, type MessageView } from "./Message";

afterEach(() => { cleanup(); window.localStorage.clear(); });

function content(message: MessageView) {
  return <LocaleProvider><Message message={message} /></LocaleProvider>;
}

describe("message disclosures", () => {
  it.each(["reasoning", "progress"] as const)("preserves the user's %s choice through updates and completion, then resets for a new message", async (kind) => {
    const user = userEvent.setup();
    const message: MessageView = {
      key: `${kind}-1`, kind, label: "Working", text: "检查账单格式", status: "streaming",
    };
    const view = render(content(message));
    const disclosure = screen.getByText("Working").closest("details") as HTMLDetailsElement;
    expect(disclosure.open).toBe(false);
    expect(disclosure.querySelector("summary")?.textContent).not.toContain("streaming");
    await user.click(screen.getByText("Show details"));
    await waitFor(() => expect(disclosure.open).toBe(true));

    view.rerender(content({ ...message, text: "检查账单格式，正在核对总额" }));
    expect(disclosure.open).toBe(true);
    view.rerender(content({ ...message, text: "核对完成", status: undefined }));
    expect(disclosure.open).toBe(true);
    await user.click(screen.getByText("Hide details"));
    await waitFor(() => expect(disclosure.open).toBe(false));

    view.rerender(content({ ...message, text: "再次检查参与者" }));
    expect(disclosure.open).toBe(false);
    view.rerender(content({ ...message, text: "参与者核对完成", status: undefined }));
    expect(disclosure.open).toBe(false);
    await user.click(screen.getByText("Show details"));
    await waitFor(() => expect(disclosure.open).toBe(true));
    view.rerender(content({ ...message, key: `${kind}-2`, text: "新消息" }));
    await waitFor(() => expect(disclosure.open).toBe(false));
  });

  it("does not expose a collapsed reasoning preview", () => {
    const text = "A long internal explanation ".repeat(30);
    const view = render(content({ key: "reasoning-1", kind: "reasoning", label: "Working", text, status: "streaming" }));
    expect(view.container.querySelector("details")?.open).toBe(false);
    expect(view.container.querySelector(".message-disclosure-preview")).toBeNull();
    expect(view.container.querySelector("summary")?.textContent).not.toContain("streaming");
  });

  it("keeps a bounded progress preview and preserves a failure status", () => {
    const text = `已读取账单\n正在检查金额\n${"参与者".repeat(150)}\n不进入短预览`;
    const view = render(content({ key: "progress-1", kind: "progress", label: "Progress", text, status: "failed" }));
    const disclosure = view.container.querySelector("details") as HTMLDetailsElement;
    const preview = disclosure.querySelector(".message-disclosure-preview");
    expect(disclosure.open).toBe(false);
    expect(preview?.textContent).toContain("已读取账单");
    expect(preview?.textContent).not.toContain("不进入短预览");
    expect(preview?.textContent?.length).toBeLessThanOrEqual(360);
    expect(disclosure.querySelector("summary")?.textContent).toContain("failed");
  });

  it("keeps the final answer directly visible while its body streams", () => {
    const message: MessageView = { key: "answer-1", kind: "assistant", label: "Sigil", text: "总额为", status: "streaming" };
    const view = render(content(message));
    expect(view.container.querySelector("details")).toBeNull();
    expect(view.container.querySelector("article.message-assistant .message-content")?.textContent).toContain("总额为");
    view.rerender(content({ ...message, text: "总额为 198.01 元" }));
    expect(view.container.querySelector("article.message-assistant .message-content")?.textContent).toContain("总额为 198.01 元");
    view.rerender(content({ ...message, text: "总额为 198.01 元", status: undefined }));
    expect(view.container.querySelector("article.message-assistant .message-content")?.textContent).toContain("总额为 198.01 元");
  });
});
