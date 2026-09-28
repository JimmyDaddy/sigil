import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationQueuePanel } from "./ConversationQueuePanel";
import { LocaleProvider } from "./i18n";
import type { ConversationQueueCommandAction, ConversationQueueView, ForegroundRunOwner } from "./types";

afterEach(cleanup);

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
