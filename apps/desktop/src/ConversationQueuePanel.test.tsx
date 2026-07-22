import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationQueuePanel } from "./ConversationQueuePanel";
import { LocaleProvider } from "./i18n";
import type { ConversationQueueCommandAction, ConversationQueueView } from "./types";

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

function renderQueue(onCommand = vi.fn(async (_action: ConversationQueueCommandAction) => true)) {
  render(
    <LocaleProvider>
      <ConversationQueuePanel
        queue={queue}
        busy={false}
        error={false}
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

  it("emits explicit durable reorder and remove commands", async () => {
    const user = userEvent.setup();
    const onCommand = renderQueue();

    const moveUpButtons = screen.getAllByRole("button", { name: "Move queued message up" });
    await user.click(moveUpButtons[1]!);
    expect(onCommand).toHaveBeenCalledWith({
      action: "reorder",
      entryId: "queue-entry-ready",
      afterEntryId: undefined,
    });

    const removeButtons = screen.getAllByRole("button", { name: "Remove queued message" });
    await user.click(removeButtons[1]!);
    expect(onCommand).toHaveBeenCalledWith({
      action: "remove",
      entryId: "queue-entry-ready",
    });
  });
});
