import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationRecoveryPanel } from "./ConversationRecoveryPanel";
import { LocaleProvider } from "./i18n";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("ConversationRecoveryPanel compaction choices", () => {
  it("keeps local preview separate from billed summary and standalone shrink", async () => {
    const prepare = vi.fn(async () => undefined);
    const shrink = vi.fn(async () => undefined);
    const apply = vi.fn(async () => undefined);
    render(
      <LocaleProvider>
        <ConversationRecoveryPanel
          recovery={{ checkpoints: [], forkPoints: [], throughStreamSequence: 9 }}
          compaction={{
            previewId: "preview-local",
            foldedEventCount: 6,
            retainedEventCount: 2,
            details: {
              activeObjective: "finish RFC-0057",
              objectiveSourceEventId: "event-1",
              activeConstraints: [],
              foldedCompleteTurnCount: 3,
              foldedTokenUpperBound: 12_000,
              retainedCompleteTurnCount: 1,
              retainedTokenUpperBound: 2_000,
              toolArtifactCount: 1,
              toolArtifacts: [{
                sourceEventId: "event-tool-1",
                contentSha256: `sha256:${"a".repeat(64)}`,
                toolName: "cargo_test",
                toolCallId: "call-1",
                status: "completed",
                originalContentBytes: 40_000,
                originalContentTokenUpperBound: 10_000,
                headExcerpt: "first lines",
                tailExcerpt: "last lines",
                reason: "large_completed_historical_result",
                recoveryInstruction: "Re-read durable transcript event event-tool-1.",
              }],
              pendingWorkCount: 1,
              unresolvedQuestionCount: 0,
              recoverableAttachmentCount: 0,
              protectedControlEventCount: 1,
              protectedActiveToolOrApprovalCount: 0,
            },
            admission: {
              kind: "prepared",
              standaloneToolOutputShrinkAvailable: true,
            },
          }}
          busy={false}
          error={false}
          onRefresh={vi.fn()}
          onPreviewCompaction={vi.fn()}
          onPrepareCompaction={prepare}
          onApplyStandaloneToolOutputShrink={shrink}
          onApplyCompaction={apply}
          onPreview={vi.fn(async () => undefined)}
          onRestore={vi.fn(async () => undefined)}
          onFork={vi.fn(async () => undefined)}
        />
      </LocaleProvider>,
    );

    expect(screen.getByText(/No provider request has been sent/i)).toBeTruthy();
    expect(screen.getByText(/cargo_test/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Generate semantic summary" }));
    expect(prepare).toHaveBeenCalledOnce();
    expect(apply).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Clean tool outputs only" }));
    expect(shrink).toHaveBeenCalledOnce();
  });
});
