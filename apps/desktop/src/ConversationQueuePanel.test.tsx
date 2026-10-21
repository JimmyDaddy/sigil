import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationQueuePanel } from "./ConversationQueuePanel";
import { LocaleProvider } from "./i18n";
import type { ConversationQueueCommandAction, ConversationQueueView, ForegroundRunOwner } from "./types";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const queue: ConversationQueueView = {
  schemaVersion: 1,
  sessionId: "session-queue",
  generation: "8",
  paused: false,
  totalItems: 2,
  truncated: false,
  nextDispatchableEntryId: "queue-entry-redacted",
  items: [
    {
      entryId: "queue-entry-redacted",
      order: 0,
      kind: "chat",
      status: "queued",
      promptPreview: "Sensitive prompt must be re-entered",
      promptPreviewTruncated: true,
      promptMaterial: "requires_reentry",
      dispatchable: false,
      blockedReason: "requires_reentry",
    },
    {
      entryId: "queue-entry-ready",
      order: 1,
      kind: "chat",
      status: "queued",
      promptPreview: "Run the focused tests",
      promptPreviewTruncated: false,
      promptMaterial: "persisted_safe",
      dispatchable: true,
    },
  ],
};

function renderQueue(
  onCommand = vi.fn(async (_action: ConversationQueueCommandAction) => true),
  view = queue,
  foregroundOwner?: ForegroundRunOwner,
) {
  render(
    <LocaleProvider>
      <ConversationQueuePanel
        queue={view}
        busy={false}
        error={false}
        foregroundOwner={foregroundOwner}
        reasoningEffort="high"
        onRefresh={() => undefined}
        onCommand={onCommand}
      />
    </LocaleProvider>,
  );
  return onCommand;
}

describe("conversation queue panel", () => {
  it("describes separate turns after the current response while retaining the foreground block", () => {
    renderQueue(undefined, {
      ...queue,
      items: [{
        ...queue.items[1]!,
        order: 0,
        blockedReason: "foreground_run_active",
      }],
      totalItems: 1,
    });

    expect(screen.getByText("Each message runs as a separate turn after the current response finishes")).toBeTruthy();
    expect(screen.getByText("Waiting for the current response to finish")).toBeTruthy();
  });

  it("requires a blank exact re-entry instead of prefilling a redacted preview", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();

    await user.click(screen.getByRole("button", { name: "Re-enter exact prompt" }));
    const input = screen.getByRole("textbox", { name: "Re-enter exact prompt" }) as HTMLTextAreaElement;
    expect(input.value).toBe("");
    expect(input.value).not.toContain("Sensitive prompt");

    await user.type(input, "Exact replacement supplied by the user");
    await user.click(screen.getByRole("button", { name: "Save and make runnable" }));

    await waitFor(() => expect(onCommand).toHaveBeenCalledWith({
      action: "edit",
      entryId: "queue-entry-redacted",
      prompt: "Exact replacement supplied by the user",
      reasoningEffort: "high",
    }));
  });

  it("prefills only a complete persisted-safe prompt for editing", async () => {
    const user = userEvent.setup();
    renderQueue();

    const editButtons = screen.getAllByRole("button", { name: "Replace queued message" });
    await user.click(editButtons[0]!);
    const input = screen.getByRole("textbox", { name: "Replacement prompt" }) as HTMLTextAreaElement;
    expect(input.value).toBe("Run the focused tests");
    expect(screen.queryByText(/The full prompt is not shown here/)).toBeNull();
  });

  it("keeps process-local exact and truncated prompts blank with a full-replacement explanation", async () => {
    const user = userEvent.setup();
    renderQueue(undefined, {
      ...queue,
      items: [{
        ...queue.items[1]!,
        promptMaterial: "available_process_local",
        promptPreview: "Partial safe preview",
        promptPreviewTruncated: false,
      }],
      totalItems: 1,
    });

    await user.click(screen.getByRole("button", { name: "Replace queued message" }));
    const input = screen.getByRole("textbox", { name: "Replacement prompt" }) as HTMLTextAreaElement;
    expect(input.value).toBe("");
    expect(screen.getByText(/The full prompt is not shown here/)).toBeTruthy();

    cleanup();
    renderQueue(undefined, {
      ...queue,
      items: [{
        ...queue.items[1]!,
        promptPreview: "The first part of a longer prompt...",
        promptPreviewTruncated: true,
      }],
      totalItems: 1,
    });
    await user.click(screen.getByRole("button", { name: "Replace queued message" }));
    expect((screen.getByRole("textbox", { name: "Replacement prompt" }) as HTMLTextAreaElement).value).toBe("");
    expect(screen.getByText(/The full prompt is not shown here/)).toBeTruthy();
  });

  it("emits explicit durable remove commands", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();

    const removeButtons = screen.getAllByRole("button", { name: "Remove queued message" });
    await user.click(removeButtons[1]!);
    expect(onCommand).toHaveBeenCalledWith({
      action: "remove",
      entryId: "queue-entry-ready",
    });
  });

  it("moves a later item next without reordering the already-first item", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();

    const runNext = screen.getByRole("button", { name: "Run this follow-up next" });
    await user.click(runNext);
    await waitFor(() => expect(onCommand).toHaveBeenCalledWith({
      action: "reorder", entryId: "queue-entry-ready",
    }));
    expect(onCommand).toHaveBeenCalledOnce();
  });

  it("resumes a paused queue after moving a later item next", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue(undefined, { ...queue, paused: true });

    await user.click(screen.getByRole("button", { name: "Run this follow-up next" }));
    await waitFor(() => expect(onCommand).toHaveBeenCalledTimes(2));
    expect(onCommand.mock.calls.map(([action]) => action)).toEqual([
      { action: "reorder", entryId: "queue-entry-ready" },
      { action: "resume" },
    ]);
  });

  it("binds interruption to the observed run owner after selecting the next item", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue(undefined, queue, {
      runId: "foreground-run", ownerRevision: "owner-revision",
    });

    await user.click(screen.getByRole("button", { name: "Interrupt and run next" }));
    await waitFor(() => expect(onCommand).toHaveBeenCalledTimes(2));
    expect(onCommand.mock.calls.map(([action]) => action)).toEqual([
      { action: "reorder", entryId: "queue-entry-ready" },
      { action: "interrupt_and_run_next", foregroundRunId: "foreground-run", foregroundOwnerRevision: "owner-revision" },
    ]);
  });

  it("does not offer main-run interruption for a non-chat queue item", () => {
    renderQueue(undefined, {
      ...queue,
      items: [{ ...queue.items[1]!, kind: "agent_message", order: 0 }],
      totalItems: 1,
    }, { runId: "foreground-run", ownerRevision: "owner-revision" });

    expect(screen.queryByRole("button", { name: "Interrupt and run next" })).toBeNull();
  });
});

describe("queued Markdown source", () => {
  it("preserves a complete prefilled source on replace without trimming code indentation or ending whitespace", async () => {
    const user = userEvent.setup();
    const source = "    code block\r\n\r\nlast line  \r\n";
    const onCommand = renderQueue(undefined, {
      ...queue,
      items: [{ ...queue.items[1]!, promptPreview: source }],
      totalItems: 1,
    });
    await user.click(screen.getByRole("button", { name: "Replace queued message" }));
    await user.click(screen.getByRole("button", { name: "Replace" }));
    await waitFor(() => expect(onCommand).toHaveBeenCalledWith({
      action: "edit",
      entryId: "queue-entry-ready",
      prompt: source,
      reasoningEffort: "high",
    }));
  });

  it("submits newly edited source exactly as entered", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();
    await user.click(screen.getByRole("button", { name: "Replace queued message" }));
    const source = "\n    中文代码\n\nlast line  \n";
    fireEvent.change(screen.getByRole("textbox", { name: "Replacement prompt" }), { target: { value: source } });
    await user.click(screen.getByRole("button", { name: "Replace" }));
    await waitFor(() => expect(onCommand).toHaveBeenCalledWith({
      action: "edit",
      entryId: "queue-entry-ready",
      prompt: source,
      reasoningEffort: "high",
    }));
  });

  it("still rejects an all-whitespace replacement through both button and form submission", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();
    await user.click(screen.getByRole("button", { name: "Replace queued message" }));
    const input = screen.getByRole("textbox", { name: "Replacement prompt" });
    fireEvent.change(input, { target: { value: "  \n\t" } });
    expect((screen.getByRole("button", { name: "Replace" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.submit(input.closest("form")!);
    expect(onCommand).not.toHaveBeenCalled();
  });
});

it.each(["en", "zh-CN"] as const)("explains the ordinary main queue in %s without implying injection into the active response", (locale) => {
  window.localStorage.setItem("sigil.desktop.locale.v1", locale);
  const onCommand = renderQueue(undefined, { ...queue, items: [], totalItems: 0 });
  expect(screen.getByText(locale === "en"
    ? "Each message runs as a separate turn after the current response finishes"
    : "当前回复结束后，按顺序单独处理每条消息")).toBeTruthy();
  expect(screen.getByText(locale === "en"
    ? "While Sigil is responding, Enter queues your next message without interrupting it."
    : "回复进行时，按 Enter 将下一条消息加入队列，不会打断当前回复。")).toBeTruthy();
  expect(screen.getByRole("button", { name: locale === "en" ? "Pause queue" : "暂停队列" })).toBeTruthy();
  expect(onCommand).not.toHaveBeenCalled();
});
