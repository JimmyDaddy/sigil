import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationPanel } from "./ConversationPanel";
import type { DesktopBridge } from "./bridge";
import { LocaleProvider } from "./i18n";
import type { ConversationDisplayPage, SessionSummary, TimelineEvent, UserInputDecisionSummary, UserInputRequest } from "./types";
import { NotificationProvider } from "./ui/feedback";
import { createCatalogWorkbenchBridge } from "./ui/catalog/workbenchBridge";

afterEach(() => { cleanup(); window.localStorage.clear(); vi.restoreAllMocks(); });

const workspaceId = "input-workspace";
const session: SessionSummary = { id: "http-input-session", runCount: 1 };
const continuation = "exact-answer-continuation";
const internalHint = "Continue after answering a requested question";
const finalText = "Read README.md: 社团活动待办工具，周末读书会。";
const answer = "周末读书会";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, decline) => { resolve = accept; reject = decline; });
  return { promise, resolve, reject };
}

function harness() {
  let listener: ((event: TimelineEvent) => void) | undefined;
  let completed = false;
  const request: UserInputRequest = {
    identity: { sessionScopeId: "durable-input-scope", rootLogicalRunId: "original-run", sourceThreadId: "main",
      requestId: "exact-question", generation: 1, sourceBindingHash: `sha256:${"a".repeat(64)}` },
    requestHash: `sha256:${"b".repeat(64)}`, source: { kind: "agent" }, purpose: "clarification",
    prompt: "活动名称", questions: [{ id: "activity", question: "活动名称", required: true, options: [], multiple: false }],
    allowedActions: ["submit", "cancel_run"], requestedAtUnixMs: 1, status: "requested",
  };
  const receipt: UserInputDecisionSummary = {
    commandId: "exact-answer-command", clientId: "desktop", sessionId: session.id, continuationRunId: continuation, replayed: false,
    request: { ...request, status: "decision_accepted", answerReceipt: { commandId: "exact-answer-command",
      decision: "submitted", answerHash: `sha256:${"c".repeat(64)}`, answeredQuestionIds: ["activity"] } },
  };
  const pending = deferred<UserInputDecisionSummary>();
  const userInputDecision = vi.fn<DesktopBridge["userInputDecision"]>(() => pending.promise);
  const display = vi.fn<DesktopBridge["display"]>(async (_workspace, sessionId): Promise<ConversationDisplayPage> => ({
    schemaVersion: 1, requestScope: `scope-${sessionId}`, throughSessionStreamSequence: completed ? "100" : "40",
    totalItems: completed ? "4" : "1", hasMore: false, gapFacts: [],
    userInputs: sessionId === session.id && !completed ? [request] : [],
    items: [{ schemaVersion: 1, displayId: `durable-user-${sessionId}`, sourceEventId: `user-source-${sessionId}`,
      displayOrder: { sessionStreamSequence: "12", subindex: 0 }, kind: "user_message", source: "durable_transcript",
      runId: continuation, status: "recorded", content: { type: "message", role: "user", text: internalHint,
        imageAttachmentCount: 0, truncated: false, originalContentBytes: internalHint.length } },
    ...(completed ? [{ schemaVersion: 1 as const, displayId: "answer-result", sourceEventId: "answer-result-source",
      displayOrder: { sessionStreamSequence: "52", subindex: 0 }, kind: "tool" as const, source: "durable_transcript" as const,
      runId: "original-run", status: "completed" as const, content: { type: "tool" as const,
        callId: "question-call", toolName: "request_user_input", output: JSON.stringify({ decision: "submitted", answers: [answer] }),
        truncated: false, originalContentBytes: answer.length } },
    { schemaVersion: 1 as const, displayId: "final-answer", sourceEventId: "final-answer-source",
      displayOrder: { sessionStreamSequence: "99", subindex: 0 }, kind: "assistant_message" as const, source: "durable_transcript" as const,
      runId: continuation, status: "succeeded" as const, reconciles: ["live-final-answer"],
      content: { type: "message" as const, role: "assistant" as const, assistantPhase: "final_answer" as const,
        text: finalText, imageAttachmentCount: 0, truncated: false, originalContentBytes: finalText.length } },
    { schemaVersion: 1 as const, displayId: "final-terminal", sourceEventId: "final-terminal-source",
      displayOrder: { sessionStreamSequence: "100", subindex: 0 }, kind: "terminal" as const, source: "durable_run_event" as const,
      runId: continuation, status: "succeeded" as const, content: { type: "terminal" as const, finalMessageId: "final-answer", summaryTruncated: false } }] : [])],
    terminalFrontier: completed ? { runId: continuation, sessionStreamSequence: "100", status: "succeeded" }
      : { runId: "original-run", sessionStreamSequence: "40", status: "awaiting_user_input" },
  }));
  const bridge: DesktopBridge = {
    ...createCatalogWorkbenchBridge("sigil_dark"), display, userInputDecision,
    continuity: async () => ({ durableFrontier: { throughStreamSequence: completed ? 100 : 40 }, retainedTerminalRuns: [], recoveryActions: [] }),
    subscribeRunEvents: async (next) => { listener = next; return () => { if (listener === next) listener = undefined; }; },
    subscribeRunStreamStatus: async () => () => undefined,
  };
  const emit = (kind: TimelineEvent["kind"], runId = continuation, text = internalHint, sequence = 1, sessionId = session.id) => {
    listener?.({ workspaceId, sessionId, runId, sequence, runSequence: String(sequence), replayable: true, kind, text,
      provisionalId: kind === "run_started" ? `live-user-${runId}` : kind === "assistant_message" ? "live-final-answer" : undefined,
      assistantKind: kind === "assistant_message" ? "final_answer" : undefined });
  };
  const finish = () => {
    completed = true;
    emit("assistant_message", continuation, finalText, 25);
    emit("run_finished", continuation, finalText, 28);
  };
  return { bridge, receipt, pending, userInputDecision, display, emit, finish, request };
}

function panel(bridge: DesktopBridge, selected = session) {
  return <LocaleProvider><NotificationProvider><ConversationPanel bridge={bridge} workspaceId={workspaceId}
    session={selected} onNewSession={async () => false} onOpenWorkspacePicker={() => undefined}
    onOpenSessionPicker={() => undefined} onOpenSettings={() => undefined} onOpenSupport={() => undefined}
    onOpenFork={async () => undefined} /></NotificationProvider></LocaleProvider>;
}

async function submit(test: ReturnType<typeof harness>) {
  const user = userEvent.setup();
  await user.type(await screen.findByRole("textbox", { name: "活动名称" }), answer);
  await user.click(screen.getByRole("button", { name: "Submit and continue" }));
  await waitFor(() => expect(test.userInputDecision).toHaveBeenCalledOnce());
}

function userMessages() { return document.querySelectorAll(".timeline .message-user"); }

describe("typed answer continuation presentation", () => {
  it.each(["event-first", "receipt-first"] as const)("retires only the synthetic user slot with %s ordering", async (order) => {
    const test = harness();
    render(panel(test.bridge));
    await submit(test);
    if (order === "event-first") act(() => test.emit("run_started"));
    await act(async () => { test.pending.resolve(test.receipt); await test.pending.promise; });
    if (order === "receipt-first") act(() => test.emit("run_started"));
    await waitFor(() => expect(userMessages()).toHaveLength(1));
    // Even the exact same words, in a durable user message for the same run, stay visible.
    expect(userMessages()[0]?.textContent).toContain(internalHint);
    act(() => test.finish());
    expect(await screen.findByText(finalText)).toBeTruthy();
    await waitFor(() => expect(document.querySelector('[data-display-id="answer-result"]')).not.toBeNull());
    await userEvent.setup().click(screen.getByRole("button", { name: "Expand request_user_input output" }));
    expect(document.querySelector('[data-display-id="answer-result"]')?.textContent).toContain(answer);
    expect(userMessages()).toHaveLength(1);
    expect(screen.getAllByText(finalText)).toHaveLength(1);
  });

  it("uses the exact run identity regardless of hint language and preserves other live user slots", async () => {
    const test = harness(); render(panel(test.bridge)); await submit(test);
    await act(async () => { test.pending.resolve(test.receipt); await test.pending.promise; });
    act(() => {
      test.emit("run_started", continuation, "继续处理已保存的答案");
      test.emit("run_started", "unrelated-run", internalHint);
    });
    await waitFor(() => expect(userMessages()).toHaveLength(2));
    expect(document.querySelector('[data-display-id="live-user-exact-answer-continuation"]')).toBeNull();
    expect(document.querySelector('[data-display-id="live-user-unrelated-run"]')).not.toBeNull();
  });

  it.each(["session", "request", "source"] as const)("does not accept a continuation receipt with a different %s binding", async (mismatch) => {
    const test = harness(); render(panel(test.bridge)); await submit(test);
    const receipt = mismatch === "session" ? { ...test.receipt, sessionId: "other-session" }
      : mismatch === "request" ? { ...test.receipt, request: { ...test.receipt.request, requestHash: `sha256:${"d".repeat(64)}` } }
        : { ...test.receipt, request: { ...test.receipt.request, identity: { ...test.receipt.request.identity,
          sourceBindingHash: `sha256:${"d".repeat(64)}` } } };
    await act(async () => { test.pending.resolve(receipt); await test.pending.promise; });
    act(() => test.emit("run_started"));
    await waitFor(() => expect(userMessages()).toHaveLength(2));
    expect(await screen.findByRole("button", { name: "Submit and continue" })).toBeTruthy();
    expect(screen.getByText("The request changed or the answer could not be accepted. Reload the latest request.")).toBeTruthy();
  });

  it("keeps an uncertain decision from retiring a user slot", async () => {
    const test = harness(); render(panel(test.bridge)); await submit(test);
    act(() => test.emit("run_started"));
    await act(async () => { test.pending.reject({ code: "command_delivery_uncertain" }); await test.pending.promise.catch(() => undefined); });
    expect(userMessages()).toHaveLength(2);
    expect(await screen.findByRole("button", { name: "Submit and continue" })).toBeTruthy();
  });

  it("ignores the old answer receipt after selecting another session in the same panel", async () => {
    const test = harness(); const view = render(panel(test.bridge)); await submit(test);
    const other = { ...session, id: "other-selected-session" };
    view.rerender(panel(test.bridge, other));
    await waitFor(() => expect(test.display.mock.calls.some(([, id]) => id === other.id)).toBe(true));
    await act(async () => { test.pending.resolve(test.receipt); await test.pending.promise; });
    act(() => test.emit("run_started", continuation, internalHint, 1, other.id));
    await waitFor(() => expect(userMessages()).toHaveLength(2));
    expect(document.querySelector('[data-display-id="live-user-exact-answer-continuation"]')).not.toBeNull();
  });
});
