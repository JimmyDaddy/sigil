import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationRecoveryPanel } from "./ConversationRecoveryPanel";
import { LocaleProvider } from "./i18n";
import type { CheckpointRestoreReview, CheckpointView } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("ConversationRecoveryPanel compaction action", () => {
  it("offers one direct compaction action without preview or apply confirmation", async () => {
    const compact = vi.fn(async () => true);
    render(
      <LocaleProvider>
        <ConversationRecoveryPanel
          recovery={{ checkpoints: [], forkPoints: [], throughStreamSequence: 9 }}
          busy={false}
          error={false}
          onRefresh={vi.fn()}
          onCompact={compact}
          onReview={vi.fn()}
          onPreview={vi.fn(async () => undefined)}
          onRestore={vi.fn(async () => undefined)}
          onFork={vi.fn(async () => undefined)}
        />
      </LocaleProvider>,
    );

    expect(screen.queryByRole("button", { name: /preview compaction/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /apply compaction/i })).toBeNull();
    expect(screen.getByText("Compact context", { selector: "summary span" }).closest("details")?.open).toBe(false);
    await userEvent.click(screen.getByText("Compact context", { selector: "summary span" }));
    await userEvent.click(screen.getByRole("button", { name: "Compact now" }));
    expect(compact).toHaveBeenCalledOnce();
  });

  it("maps loading and failed recovery states and keeps retry explicit", async () => {
    const user = userEvent.setup();
    const onRefresh = vi.fn();
    const props = {
      busy: false,
      error: false,
      onRefresh,
      onCompact: vi.fn(async () => true),
      onReview: vi.fn(),
    onPreview: vi.fn(async () => undefined),
      onRestore: vi.fn(async () => undefined),
      onFork: vi.fn(async () => undefined),
    };

    const { rerender } = render(
      <LocaleProvider>
        <ConversationRecoveryPanel {...props} />
      </LocaleProvider>,
    );
    expect(screen.getByTestId("recovery-state").getAttribute("data-recovery-product-status")).toBe("loading");
    expect(screen.getByText("Loading")).toBeTruthy();

    rerender(
      <LocaleProvider>
        <ConversationRecoveryPanel {...props} error />
      </LocaleProvider>,
    );
    expect(screen.getByTestId("recovery-state").getAttribute("data-recovery-product-status")).toBe("failed");
    expect(screen.getByText("Failed")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRefresh).toHaveBeenCalledOnce();
  });
});

describe("recovery hierarchy and exact evidence", () => {
  const callbacks = () => ({ busy: false, error: false, onRefresh: vi.fn(), onCompact: vi.fn(async () => true), onReview: vi.fn(), onPreview: vi.fn(async () => undefined), onRestore: vi.fn(async () => undefined), onFork: vi.fn(async () => undefined) });

  it("puts the known checkpoint state before the retained compaction action and omits an empty fork container", () => {
    const { container } = render(<ConversationRecoveryPanel {...callbacks()} recovery={{ checkpoints: [], forkPoints: [], throughStreamSequence: 9 }} />);
    const checkpoints = screen.getByRole("heading", { name: "Checkpoints" });
    const compact = screen.getByText("Compact context", { selector: "summary span" });
    expect(checkpoints.compareDocumentPosition(compact) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByText("No durable controlled-file checkpoints are available.")).toBeTruthy();
    expect(screen.queryByText("Conversation forks")).toBeNull();
    expect(container.querySelectorAll(".compaction-review h3")).toHaveLength(0);
    expect(compact.closest("details")?.open).toBe(false);
  });

  it("retains nonempty fork actions without a second title or weakening the exact source digest", async () => {
    const user = userEvent.setup();
    const props = callbacks();
    render(<ConversationRecoveryPanel {...props} recovery={{ checkpoints: [], throughStreamSequence: 9,
      forkPoints: [{ sourceTurnIndex: 1, sourceTurnDigest: "exact-finalized-source", sourceBoundaryStreamSequence: 3, sourceFinalizedStreamSequence: 7, promptPreview: "Inspect this exact turn" }] }} />);
    const title = screen.getByText("Conversation forks", { selector: "summary span" });
    expect(screen.getAllByText("Conversation forks")).toHaveLength(1);
    await user.click(title);
    expect(screen.getByText("Inspect this exact turn")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Fork conversation" }));
    expect(props.onFork).toHaveBeenCalledExactlyOnceWith("exact-finalized-source");
  });

  it("keeps complete conflict evidence and rejects a mismatched checkpoint review", async () => {
    const user = userEvent.setup();
    const props = callbacks();
    const checkpoint: CheckpointView = { checkpointId: "checkpoint", checkpointDigest: "exact-digest", turnIndex: 1, files: [], unknownMutationCount: 1, fullyRestorable: false };
    const preview: CheckpointRestoreReview = { checkpointId: checkpoint.checkpointId, checkpointDigest: checkpoint.checkpointDigest,
      files: [{ path: "src/source.ts", restoreKind: "restore_content", conflictReason: "current_hash_mismatch" }],
      reverseDiffs: [{ path: "src/source.ts", diff: "EXACT FIRST\n-old\n+new\nEXACT TAIL", truncated: true, originalLineCount: 40 }], unknownMutationCount: 1, ready: false };
    const view = (review: CheckpointRestoreReview) => <ConversationRecoveryPanel {...props} recovery={{ checkpoints: [checkpoint], forkPoints: [], throughStreamSequence: 9 }} preview={review} />;
    const { rerender } = render(view(preview));
    await user.click(screen.getByRole("button", { name: "Preview restore" }));
    expect(props.onPreview).toHaveBeenCalledExactlyOnceWith(checkpoint);
    const review = screen.getByRole("heading", { name: "Reverse diff preview" }).closest("section")!;
    expect(review.querySelector("pre code")?.textContent).toBe(preview.reverseDiffs[0].diff);
    expect((within(review).getByRole("button", { name: "Restore controlled files" }) as HTMLButtonElement).disabled).toBe(true);
    expect(within(review).getByText("current_hash_mismatch")).toBeTruthy();
    rerender(view({ ...preview, checkpointDigest: "different-digest", ready: true }));
    expect(screen.queryByRole("button", { name: "Restore controlled files" })).toBeNull();
    expect(props.onRestore).not.toHaveBeenCalled();
  });
});

describe("failed recovery with retained information", () => {
  const checkpoint: CheckpointView = { checkpointId: "retained-checkpoint", checkpointDigest: "retained-digest", turnIndex: 2, prompt: "The previously loaded turn", files: [], unknownMutationCount: 0, fullyRestorable: false };
  const recovery = { checkpoints: [checkpoint], forkPoints: [], throughStreamSequence: 9 };
  const callbacks = () => ({ busy: false, onRefresh: vi.fn(), onCompact: vi.fn(async () => true), onReview: vi.fn(), onPreview: vi.fn(async () => undefined), onRestore: vi.fn(async () => undefined), onFork: vi.fn(async () => undefined) });

  it("shows a nonblocking error and retry alongside the old checkpoint instead of hiding either", async () => {
    const user = userEvent.setup();
    const props = callbacks();
    const { rerender } = render(<ConversationRecoveryPanel {...props} recovery={recovery} error />);
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("Previously loaded information is preserved");
    expect(screen.getByText(checkpoint.prompt!)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Preview restore" })).toBeTruthy();
    await user.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(props.onRefresh).toHaveBeenCalledOnce();
    expect(props.onPreview).not.toHaveBeenCalled();
    expect(props.onRestore).not.toHaveBeenCalled();
    rerender(<ConversationRecoveryPanel {...props} busy recovery={recovery} error />);
    expect((within(screen.getByRole("alert")).getByRole("button", { name: "Retry" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(checkpoint.prompt!)).toBeTruthy();
  });

  it("does not recreate a failed file preview merely because loading the recovery view later succeeds", async () => {
    const user = userEvent.setup();
    const props = callbacks();
    const preview: CheckpointRestoreReview = { checkpointId: checkpoint.checkpointId, checkpointDigest: checkpoint.checkpointDigest, files: [], reverseDiffs: [], unknownMutationCount: 0, ready: true };
    const view = (error: boolean, review?: CheckpointRestoreReview) => <ConversationRecoveryPanel {...props} recovery={recovery} preview={review} error={error} />;
    const { rerender } = render(view(false, preview));
    await user.click(screen.getByRole("button", { name: "Preview restore" }));
    expect(screen.getByRole("button", { name: "Restore controlled files" })).toBeTruthy();
    // The real preview catch clears its failed review and retains the loaded recovery view.
    rerender(view(true));
    expect(screen.getByRole("alert").textContent).toContain("retry any failed preview separately");
    expect(screen.getByText(checkpoint.prompt!)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Restore controlled files" })).toBeNull();
    await user.click(within(screen.getByRole("alert")).getByRole("button", { name: "Retry" }));
    rerender(view(false));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button", { name: "Restore controlled files" })).toBeNull();
    expect(props.onRestore).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Preview restore" }));
    expect(props.onPreview).toHaveBeenLastCalledWith(checkpoint);
    expect(props.onPreview).toHaveBeenCalledTimes(2);
  });
});
