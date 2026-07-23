import { describe, expect, it } from "vitest";

import {
  createConversationContinuityState,
  reduceConversationContinuity,
  resolveConversationIdentity,
  selectConversationTimeline,
  type ConversationDisplayPage,
  type DurableConversationDisplayItem,
  type LiveConversationDisplayItem,
} from "./continuityReducer";

const SESSION_ID = "session-1";
const REQUEST_SCOPE = "request-scope-1";

describe("conversation continuity reducer", () => {
  it("leaves transcript loading on failure and resumes owner proof after a successful retry", () => {
    let state = createConversationContinuityState(SESSION_ID);
    state = reduceConversationContinuity(state, {
      type: "initial_page_failed",
      sessionId: SESSION_ID,
      message: "projection unavailable",
    });

    expect(state.lifecycle).toBe("error");
    expect(state.transcriptLoaded).toBe(false);
    expect(state.recovery).toEqual(expect.objectContaining({ canContinueReadOnly: false }));

    state = reduceConversationContinuity(state, {
      type: "recovery_retry_started",
      sessionId: SESSION_ID,
    });
    state = reduceConversationContinuity(state, {
      type: "initial_page_received",
      sessionId: SESSION_ID,
      page: page([], "0"),
    });

    expect(state.lifecycle).toBe("checking_owner");
    expect(state.transcriptLoaded).toBe(true);
  });

  it("preserves duplicate text when stable display ids are distinct", () => {
    const state = receiveInitial([
      messageItem("message-1", "1", "same text"),
      messageItem("message-2", "2", "same text"),
    ]);

    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "message-1",
      "message-2",
    ]);
  });

  it("orders decimal u64 values beyond JavaScript's safe integer range with BigInt", () => {
    const state = receiveInitial([
      messageItem("later", "9007199254740993", "later"),
      messageItem("earlier", "9007199254740992", "earlier"),
    ]);

    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "earlier",
      "later",
    ]);
  });

  it("replaces a live item only when a durable successor names its provisional id", () => {
    let state = receiveInitial([]);
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveFinal("live-final", "run-1", "1", "answer"),
    });
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([
        assistantFinal("durable-final", "10", "run-1", "answer", ["live-final"]),
      ], "10"),
    });

    expect(selectConversationTimeline(state).map(({ identity, source }) => [identity, source])).toEqual([
      ["durable-final", "durable"],
    ]);
    expect(state.reconciledIdentities.has("live-final")).toBe(true);
    expect(resolveConversationIdentity(state, "live-final")).toBe("durable-final");
  });

  it("accepts an exact durable lifecycle chain that advances one live slot", () => {
    let state = receiveInitial([]);
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveTool("live-tool", "run-1", "4", "running", "partial"),
    });
    const requested = toolItem("tool-requested", "5", "run-1", "requested");
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([{
        ...requested,
        reconciles: ["live-tool"],
      }, {
        ...toolItem("tool-completed", "6", "run-1", "completed"),
        reconciles: [requested.displayId, "live-tool"],
      }], "6"),
    });

    expect(state.contractError).toBeUndefined();
    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "tool-completed",
    ]);
    expect(resolveConversationIdentity(state, "live-tool")).toBe("tool-completed");
    expect(resolveConversationIdentity(state, "tool-requested")).toBe("tool-completed");
  });

  it("rejects two durable successors when the later item does not extend the first", () => {
    let state = receiveInitial([{
      ...toolItem("tool-requested", "5", "run-1", "requested"),
      reconciles: ["live-tool"],
    }]);
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([{
        ...approvalItem("unrelated-approval", "6", "run-1", "approved"),
        reconciles: ["live-tool"],
      }], "6"),
    });

    expect(state.lifecycle).toBe("error");
    expect(state.contractError).toMatchObject({ code: "invalid_reconciliation" });
  });

  it("keeps a canonical contract error sticky across owner lifecycle events", () => {
    let state = receiveInitial([messageItem("message-1", "1", "safe")]);
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([messageItem("message-1", "1", "changed")], "1"),
    });
    const rejected = state;

    state = reduceConversationContinuity(state, {
      type: "owner_probe_started",
      sessionId: SESSION_ID,
    });
    state = reduceConversationContinuity(state, {
      type: "owner_probe_resolved",
      sessionId: SESSION_ID,
    });

    expect(state).toBe(rejected);
    expect(state.lifecycle).toBe("error");
    expect(state.contractError?.code).toBe("conflicting_display_id");
  });

  it("never synthesizes an assistant answer from a terminal marker", () => {
    let state = receiveInitial([]);
    const transport = { runId: "run-1", runSequence: "999", status: "succeeded" } as const;
    state = reduceConversationContinuity(state, {
      type: "terminal_observed",
      sessionId: SESSION_ID,
      terminal: { runId: transport.runId, status: transport.status },
    });

    expect(state.lifecycle).toBe("finalizing");
    expect(selectConversationTimeline(state)).toEqual([]);

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([], transport.runSequence),
    });
    expect(state.lifecycle).toBe("finalizing");
    expect(state.refreshState).toBe("needed");

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([
        assistantFinal("final", "9", "run-1", "answer"),
        terminalItem("terminal", "10", "run-1"),
      ], "10", { runId: "run-1", sessionStreamSequence: "10", status: "succeeded" }),
    });

    const timeline = selectConversationTimeline(state);
    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.identity).toBe("final");
    expect(state.lifecycle).toBe("idle");
  });

  it("keeps a status-only terminal transport finalizing until a durable terminal frontier arrives", () => {
    let state = receiveInitial([]);
    state = reduceConversationContinuity(state, {
      type: "terminal_transport_observed",
      sessionId: SESSION_ID,
      runId: "run-status-only",
    });

    expect(state.lifecycle).toBe("finalizing");
    expect(state.pendingTerminalRunId).toBe("run-status-only");
    expect(selectConversationTimeline(state)).toEqual([]);

    state = reduceConversationContinuity(state, {
      type: "owner_probe_resolved",
      sessionId: SESSION_ID,
    });
    expect(state.lifecycle).toBe("finalizing");

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([], "20"),
    });
    expect(state.lifecycle).toBe("finalizing");
    expect(state.refreshState).toBe("needed");

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([
        assistantFinal("status-only-final", "20", "run-status-only", "answer"),
      ], "21", {
        runId: "run-status-only",
        sessionStreamSequence: "21",
        status: "interrupted",
      }),
    });

    expect(state.lifecycle).toBe("idle");
    expect(state.pendingTerminalRunId).toBeUndefined();
    expect(state.canonicalTerminal?.status).toBe("interrupted");
    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "status-only-final",
    ]);
  });

  it("does not use equal text as a reconciliation identity", () => {
    let state = receiveInitial([reasoningItem("durable-reasoning", "4", "same reasoning")]);
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveReasoning("live-reasoning", "run-1", "5", "same reasoning"),
    });

    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "durable-reasoning",
      "live-reasoning",
    ]);
  });

  it("rejects duplicate final answers that lack exact reconciliation", () => {
    let state = receiveInitial([assistantFinal("durable-final", "4", "run-1", "answer")]);
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveFinal("live-final", "run-1", "5", "answer"),
    });

    expect(state.lifecycle).toBe("error");
    expect(state.contractError?.code).toBe("duplicate_final");
  });

  it("owns pagination metadata and preserves the initial live anchor across older pages", () => {
    const initialAnchor = { durableFrontier: "5", runId: "run-live", runSequence: "7" };
    let state = reduceConversationContinuity(createConversationContinuityState(SESSION_ID), {
      type: "initial_page_received",
      sessionId: SESSION_ID,
      page: page([messageItem("latest", "5", "latest")], "5", undefined, {
        totalItems: "3",
        nextCursor: "cursor-1",
        hasMore: true,
        gapFacts: [{ kind: "retention", afterSessionStreamSequence: "1" }],
        liveProvisionalAnchor: initialAnchor,
      }),
    });
    state = reduceConversationContinuity(state, {
      type: "older_page_received",
      sessionId: SESSION_ID,
      page: page([messageItem("older", "4", "older")], "5", undefined, {
        totalItems: "3",
        hasMore: false,
        gapFacts: [{ kind: "replay", afterSessionStreamSequence: "2" }],
        liveProvisionalAnchor: { durableFrontier: "5", runId: "wrong", runSequence: "99" },
      }),
    });

    expect(state.totalItems).toBe("3");
    expect(state.nextCursor).toBeUndefined();
    expect(state.hasMore).toBe(false);
    expect(state.gapFacts).toEqual([
      { kind: "retention", afterSessionStreamSequence: "1" },
      { kind: "replay", afterSessionStreamSequence: "2" },
    ]);
    expect(state.liveProvisionalAnchor).toEqual(initialAnchor);

    const refreshedAnchor = { durableFrontier: "6", runId: "run-next", runSequence: "1" };
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([messageItem("newest", "6", "newest")], "6", undefined, {
        totalItems: "4",
        nextCursor: "cursor-refresh",
        hasMore: true,
        gapFacts: [],
        liveProvisionalAnchor: refreshedAnchor,
      }),
    });
    expect(state.totalItems).toBe("4");
    expect(state.nextCursor).toBe("cursor-refresh");
    expect(state.hasMore).toBe(true);
    expect(state.gapFacts).toEqual([]);
    expect(state.liveProvisionalAnchor).toEqual(refreshedAnchor);
  });

  it("keeps terminal replay and lifecycle status monotonic", () => {
    let state = receiveInitial([]);
    const terminal = { runId: "run-1", status: "succeeded" } as const;
    state = reduceConversationContinuity(state, {
      type: "terminal_observed",
      sessionId: SESSION_ID,
      terminal,
    });
    const afterTerminal = state;
    state = reduceConversationContinuity(state, {
      type: "terminal_observed",
      sessionId: SESSION_ID,
      terminal,
    });
    expect(state).toBe(afterTerminal);

    expect(state.lifecycle).toBe("finalizing");

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([], "20", { ...terminal, sessionStreamSequence: "20" }),
    });
    expect(state.lifecycle).toBe("idle");
    expect(state.canonicalTerminal).toEqual({
      ...terminal,
      sessionStreamSequence: "20",
    });

    const settled = state;
    state = reduceConversationContinuity(state, {
      type: "terminal_observed",
      sessionId: SESSION_ID,
      terminal,
    });
    expect(state).toBe(settled);

    let conflicting = receiveInitial([]);
    conflicting = reduceConversationContinuity(conflicting, {
      type: "terminal_observed",
      sessionId: SESSION_ID,
      terminal,
    });
    conflicting = reduceConversationContinuity(conflicting, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([], "20", {
        runId: "run-1",
        sessionStreamSequence: "20",
        status: "failed",
      }),
    });
    expect(conflicting.lifecycle).toBe("error");
    expect(conflicting.contractError?.code).toBe("terminal_conflict");
  });

  it("rejects a page from a different request scope without merging it", () => {
    const initial = receiveInitial([messageItem("message-1", "1", "safe")]);
    const state = reduceConversationContinuity(initial, {
      type: "older_page_received",
      sessionId: SESSION_ID,
      page: { ...page([messageItem("wrong", "0", "wrong")], "1"), requestScope: "other" },
    });

    expect(state.lifecycle).toBe("error");
    expect(state.contractError?.code).toBe("request_scope_mismatch");
    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual(["message-1"]);
  });

  it("rejects conflicting content that reuses a durable display id", () => {
    const initial = receiveInitial([messageItem("message-1", "1", "first")]);
    const state = reduceConversationContinuity(initial, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([messageItem("message-1", "1", "changed")], "1"),
    });

    expect(state.lifecycle).toBe("error");
    expect(state.contractError?.code).toBe("conflicting_display_id");
    expect((selectConversationTimeline(state)[0]?.item.content as { text?: string }).text).toBe("first");
  });

  it("uses exact semantic replacement for tool and approval lifecycle items", () => {
    const requestedTool = toolItem("tool-requested", "5", "run-1", "requested");
    const requestedApproval = approvalItem("approval-requested", "6", "run-1", "requested");
    let state = receiveInitial([requestedTool, requestedApproval]);
    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: page([
        { ...toolItem("tool-completed", "8", "run-1", "completed"), reconciles: ["tool-requested"] },
        { ...approvalItem("approval-approved", "7", "run-1", "approved"), reconciles: ["approval-requested"] },
      ], "8"),
    });

    expect(selectConversationTimeline(state).map(({ identity }) => identity)).toEqual([
      "approval-approved",
      "tool-completed",
    ]);
    expect(state.reconciledIdentities).toEqual(new Set(["approval-requested", "tool-requested"]));
  });

  it("updates one provisional item monotonically and ignores an older replay", () => {
    let state = receiveInitial([]);
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: {
        ...liveTool("live-tool", "run-1", "4", "running", "partial"),
        toolInput: "rg TODO",
      },
    });
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveTool("live-tool", "run-1", "6", "completed", "complete"),
    });
    const completed = state;
    state = reduceConversationContinuity(state, {
      type: "live_item_received",
      sessionId: SESSION_ID,
      item: liveTool("live-tool", "run-1", "5", "running", "late replay"),
    });

    expect(state).toBe(completed);
    expect(state.liveItems.get("live-tool")?.status).toBe("completed");
    expect(state.liveItems.get("live-tool")?.content).toMatchObject({ output: "complete" });
    expect(state.liveItems.get("live-tool")?.toolInput).toBe("rg TODO");
  });

  it("treats selecting the current session as a referential no-op and resets for another session", () => {
    const current = receiveInitial([messageItem("message-1", "1", "safe")]);
    expect(reduceConversationContinuity(current, {
      type: "session_selected",
      sessionId: SESSION_ID,
    })).toBe(current);

    const next = reduceConversationContinuity(current, {
      type: "session_selected",
      sessionId: "session-2",
    });
    expect(next.sessionId).toBe("session-2");
    expect(next.lifecycle).toBe("loading_transcript");
    expect(selectConversationTimeline(next)).toEqual([]);
  });

  it("models owner admission, exact attach and read-only recovery", () => {
    let state = receiveInitial([]);
    state = reduceConversationContinuity(state, {
      type: "owner_probe_resolved",
      sessionId: SESSION_ID,
      foregroundOwner: { runId: "run-1", ownerRevision: "owner-1" },
    });
    expect(state.lifecycle).toBe("attaching_run");

    state = reduceConversationContinuity(state, {
      type: "run_attached",
      sessionId: SESSION_ID,
      runId: "run-1",
      ownerRevision: "owner-1",
    });
    expect(state.lifecycle).toBe("live");

    state = reduceConversationContinuity(state, {
      type: "owner_probe_failed",
      sessionId: SESSION_ID,
      message: "owner unavailable",
      canContinueReadOnly: true,
    });
    expect(state.lifecycle).toBe("read_only_recovery");
    state = reduceConversationContinuity(state, {
      type: "continue_read_only",
      sessionId: SESSION_ID,
    });
    expect(state.lifecycle).toBe("read_only");
  });
});

function receiveInitial(items: DurableConversationDisplayItem[]) {
  return reduceConversationContinuity(createConversationContinuityState(SESSION_ID), {
    type: "initial_page_received",
    sessionId: SESSION_ID,
    page: page(
      items,
      items[items.length - 1]?.displayOrder.sessionStreamSequence ?? "0",
    ),
  });
}

function page(
  items: DurableConversationDisplayItem[],
  throughSessionStreamSequence: string,
  terminalFrontier?: ConversationDisplayPage["terminalFrontier"],
  metadata: Partial<Pick<
    ConversationDisplayPage,
    "totalItems" | "nextCursor" | "hasMore" | "gapFacts" | "liveProvisionalAnchor"
  >> = {},
): ConversationDisplayPage {
  return {
    schemaVersion: 1,
    requestScope: REQUEST_SCOPE,
    throughSessionStreamSequence,
    totalItems: metadata.totalItems ?? String(items.length),
    items,
    terminalFrontier,
    nextCursor: metadata.nextCursor,
    hasMore: metadata.hasMore ?? metadata.nextCursor !== undefined,
    gapFacts: metadata.gapFacts ?? [],
    liveProvisionalAnchor: metadata.liveProvisionalAnchor,
  };
}

function messageItem(
  displayId: string,
  sessionStreamSequence: string,
  text: string,
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "user_message",
    source: "durable_transcript",
    status: "recorded",
    content: {
      type: "message",
      role: "user",
      text,
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: text.length,
    },
  };
}

function assistantFinal(
  displayId: string,
  sessionStreamSequence: string,
  runId: string,
  text: string,
  reconciles?: string[],
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "assistant_message",
    source: "durable_transcript",
    runId,
    status: "succeeded",
    content: {
      type: "message",
      role: "assistant",
      assistantPhase: "final_answer",
      text,
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: text.length,
    },
    reconciles,
  };
}

function reasoningItem(
  displayId: string,
  sessionStreamSequence: string,
  text: string,
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "reasoning",
    source: "durable_transcript",
    status: "recorded",
    content: {
      type: "reasoning",
      text,
      truncated: false,
      originalContentBytes: text.length,
    },
  };
}

function liveReasoning(
  provisionalId: string,
  runId: string,
  runSequence: string,
  text: string,
): LiveConversationDisplayItem {
  return {
    provisionalId,
    runId,
    runSequence,
    kind: "reasoning",
    status: "streaming",
    content: {
      type: "reasoning",
      text,
      truncated: false,
      originalContentBytes: text.length,
    },
  };
}

function liveFinal(
  provisionalId: string,
  runId: string,
  runSequence: string,
  text: string,
): LiveConversationDisplayItem {
  return {
    provisionalId,
    runId,
    runSequence,
    kind: "assistant_message",
    status: "streaming",
    content: {
      type: "message",
      role: "assistant",
      assistantPhase: "final_answer",
      text,
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: text.length,
    },
  };
}

function terminalItem(
  displayId: string,
  sessionStreamSequence: string,
  runId: string,
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "terminal",
    source: "durable_run_event",
    runId,
    status: "succeeded",
    content: { type: "terminal", finalMessageId: "final", summaryTruncated: false },
  };
}

function toolItem(
  displayId: string,
  sessionStreamSequence: string,
  runId: string,
  status: "requested" | "completed",
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "tool",
    source: "durable_run_event",
    runId,
    status,
    content: {
      type: "tool",
      callId: "call-1",
      toolName: "read_file",
      truncated: false,
      originalContentBytes: 0,
    },
  };
}

function approvalItem(
  displayId: string,
  sessionStreamSequence: string,
  runId: string,
  status: "requested" | "approved",
): DurableConversationDisplayItem {
  return {
    schemaVersion: 1,
    displayId,
    displayOrder: { sessionStreamSequence, subindex: 0 },
    sourceEventId: `event-${displayId}`,
    kind: "approval",
    source: "durable_run_event",
    runId,
    status,
    content: {
      type: "approval",
      callId: "call-1",
      toolName: "read_file",
      decision: status === "approved" ? "approved" : undefined,
    },
  };
}

function liveTool(
  provisionalId: string,
  runId: string,
  runSequence: string,
  status: "running" | "completed",
  output: string,
): LiveConversationDisplayItem {
  return {
    provisionalId,
    runId,
    runSequence,
    kind: "tool",
    status,
    content: {
      type: "tool",
      callId: "call-1",
      toolName: "read_file",
      output,
      truncated: false,
      originalContentBytes: output.length,
    },
  };
}
