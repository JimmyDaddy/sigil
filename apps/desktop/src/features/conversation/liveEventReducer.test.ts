import { describe, expect, it } from "vitest";

import type { TimelineApproval, TimelineEvent } from "../../types";
import {
  createLiveEventState,
  liveEventReducer,
  reduceLiveTimelineEvent,
  selectDeltaBuffers,
  selectDeltaText,
  selectLatestPendingApproval,
  selectSemanticLiveItems,
  selectTerminalSignals,
  semanticLiveItemFromTimelineEvent,
} from "./liveEventReducer";

const SESSION_ID = "session-1";

describe("live event reducer", () => {
  it("orders semantic items by exact decimal run sequence and ignores legacy numeric sequence", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      sequence: 999,
      runSequence: "9007199254740993",
      provisionalId: "live-later",
      kind: "assistant_message",
      text: "later",
      assistantKind: "progress",
    }));
    state = reduceLiveTimelineEvent(state, event({
      sequence: 1,
      runSequence: "9007199254740992",
      provisionalId: "live-earlier",
      kind: "assistant_message",
      text: "earlier",
      assistantKind: "tool_preamble",
    }));

    expect(selectSemanticLiveItems(state).map((item) => item.provisionalId)).toEqual([
      "live-earlier",
      "live-later",
    ]);
  });

  it("updates one semantic identity monotonically without comparing its text", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      runSequence: "4",
      provisionalId: "live-tool",
      kind: "tool_started",
      itemId: "call-1",
      toolName: "bash",
      toolInput: "rg TODO",
      text: "same",
    }));
    state = reduceLiveTimelineEvent(state, event({
      runSequence: "6",
      provisionalId: "live-tool",
      kind: "tool_result",
      itemId: "call-1",
      toolName: "bash",
      status: "ok",
      text: "changed",
    }));
    const completed = state;
    state = reduceLiveTimelineEvent(state, event({
      runSequence: "5",
      provisionalId: "live-tool",
      kind: "tool_progress",
      itemId: "call-1",
      text: "late replay",
    }));

    expect(state).toBe(completed);
    expect(selectSemanticLiveItems(state)[0]).toMatchObject({
      runSequence: "6",
      status: "succeeded",
      content: { output: "changed" },
      toolInput: "rg TODO",
    });
  });

  it("buffers id-less deltas by BigInt run sequence and ignores duplicate sequence replay", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      sequence: 1,
      runSequence: "9007199254740993",
      kind: "assistant_delta",
      text: "B",
    }));
    state = reduceLiveTimelineEvent(state, event({
      sequence: 999,
      runSequence: "9007199254740992",
      kind: "assistant_delta",
      text: "A",
    }));
    state = reduceLiveTimelineEvent(state, event({
      sequence: 2,
      runSequence: "9007199254740992",
      kind: "assistant_delta",
      text: "different replay text",
    }));

    const buffer = selectDeltaBuffers(state)[0];
    expect(buffer?.firstRunSequence).toBe("9007199254740992");
    expect(buffer?.lastRunSequence).toBe("9007199254740993");
    expect(buffer === undefined ? "" : selectDeltaText(buffer)).toBe("AB");
  });

  it("clears only the delta channel replaced by a semantic boundary", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "1", text: "draft" }));
    state = reduceLiveTimelineEvent(state, event({ kind: "reasoning_delta", runSequence: "2", text: "thought" }));
    expect(selectDeltaBuffers(state)).toHaveLength(2);

    state = reduceLiveTimelineEvent(state, event({
      kind: "assistant_message",
      runSequence: "3",
      provisionalId: "assistant-1",
      assistantKind: "tool_preamble",
      text: "complete",
    }));
    expect(selectDeltaBuffers(state).map((buffer) => buffer.channel)).toEqual(["reasoning"]);

    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "4", text: "next" }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "tool_started",
      runSequence: "5",
      provisionalId: "tool-1",
      itemId: "call-1",
      toolName: "read_file",
    }));
    expect(selectDeltaBuffers(state).map((buffer) => buffer.channel)).toEqual(["reasoning"]);

    state = reduceLiveTimelineEvent(state, event({ kind: "reasoning_delta", runSequence: "6", text: " last" }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "run_finished",
      runSequence: "7",
      provisionalId: "terminal-1",
      text: "must not become a final row",
    }));
    const reasoning = selectDeltaBuffers(state)[0];
    expect(reasoning === undefined ? "" : selectDeltaText(reasoning)).toBe("thought last");

    state = liveEventReducer(state, {
      type: "anchor_received",
      sessionId: SESSION_ID,
      anchor: { durableFrontier: "10", runId: "run-1", runSequence: "7" },
    });
    expect(selectDeltaBuffers(state)).toEqual([]);
  });

  it("does not let a late semantic boundary erase newer delta fragments", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "8", text: "newer" }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "assistant_message",
      runSequence: "7",
      provisionalId: "assistant-old",
      assistantKind: "progress",
      text: "older boundary",
    }));

    const buffer = selectDeltaBuffers(state)[0];
    expect(buffer === undefined ? "" : selectDeltaText(buffer)).toBe("newer");
  });

  it("returns only a terminal signal and never a semantic final for terminal events", () => {
    let state = createLiveEventState(SESSION_ID);
    const terminal = event({
      kind: "run_finished",
      runSequence: "8",
      provisionalId: "terminal-1",
      text: "do not render me",
    });
    expect(semanticLiveItemFromTimelineEvent(terminal)).toBeUndefined();

    state = reduceLiveTimelineEvent(state, terminal);
    expect(selectSemanticLiveItems(state)).toEqual([]);
    expect(selectTerminalSignals(state)).toEqual([{
      runId: "run-1",
      runSequence: "8",
      status: "succeeded",
    }]);

    state = reduceLiveTimelineEvent(state, event({
      kind: "run_failed",
      runId: "run-interrupted",
      runSequence: "9",
      status: "interrupted",
    }));
    expect(selectTerminalSignals(state)[1]).toEqual({
      runId: "run-interrupted",
      runSequence: "9",
      status: "interrupted",
    });
  });

  it("keeps the exact pending approval guard and removes it on resolution", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "10",
      provisionalId: "approval-1",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));

    expect(selectLatestPendingApproval(state)?.approval).toEqual(approval);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "11",
      provisionalId: "approval-1",
      itemId: approval.callId,
      toolName: approval.toolName,
      status: "approved",
    }));
    expect(selectLatestPendingApproval(state)).toBeUndefined();
    const semanticItems = selectSemanticLiveItems(state);
    expect(semanticItems[semanticItems.length - 1]).toMatchObject({
      provisionalId: "approval-1",
      status: "approved",
    });
  });

  it("keeps a resolved high-water tombstone so an older request cannot revive", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "9007199254740993",
      provisionalId: "approval-resolved",
      itemId: approval.callId,
      toolName: approval.toolName,
      status: "approved",
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "9007199254740992",
      provisionalId: "approval-requested",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));

    expect(selectLatestPendingApproval(state)).toBeUndefined();
    expect([...state.approvalLifecycles.values()][0]).toMatchObject({
      runId: "run-1",
      callId: "call-1",
      runSequence: "9007199254740993",
      phase: "resolved",
    });
    expect(selectSemanticLiveItems(state).map((item) => item.provisionalId)).toEqual([
      "approval-resolved",
    ]);
  });

  it("does not let an older resolution delete a newer pending request", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "9007199254740993",
      provisionalId: "approval-requested",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "9007199254740992",
      provisionalId: "approval-resolved",
      itemId: approval.callId,
      toolName: approval.toolName,
      status: "denied",
    }));

    expect(selectLatestPendingApproval(state)?.approval).toEqual(approval);
    expect([...state.approvalLifecycles.values()][0]?.phase).toBe("pending");
    expect(selectSemanticLiveItems(state)).toHaveLength(1);
    expect(selectSemanticLiveItems(state)[0]).toMatchObject({
      provisionalId: "approval-requested",
      status: "waiting_for_approval",
    });
  });

  it("lets a same-sequence resolution win and fails closed on a conflicting guard", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "12",
      provisionalId: "approval-requested",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "12",
      provisionalId: "approval-resolved",
      itemId: approval.callId,
      toolName: approval.toolName,
      status: "approved",
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "12",
      provisionalId: "late-same-sequence-request",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));

    expect(selectLatestPendingApproval(state)).toBeUndefined();
    expect([...state.approvalLifecycles.values()][0]?.phase).toBe("resolved");
    expect(selectSemanticLiveItems(state)).toHaveLength(1);
    expect(selectSemanticLiveItems(state)[0]).toMatchObject({
      provisionalId: "approval-resolved",
      status: "approved",
    });

    const conflictingApproval = { ...approval, toolCallHash: "different-hash" };
    let conflict = createLiveEventState(SESSION_ID);
    conflict = reduceLiveTimelineEvent(conflict, event({
      kind: "approval_requested",
      runSequence: "13",
      provisionalId: "approval-original",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    }));
    conflict = reduceLiveTimelineEvent(conflict, event({
      kind: "approval_requested",
      runSequence: "13",
      provisionalId: "approval-conflict",
      itemId: conflictingApproval.callId,
      toolName: conflictingApproval.toolName,
      approval: conflictingApproval,
    }));

    expect(selectLatestPendingApproval(conflict)).toBeUndefined();
    expect([...conflict.approvalLifecycles.values()][0]?.phase).toBe("closed");
    expect(selectSemanticLiveItems(conflict)).toEqual([]);
  });

  it.each([
    ["approved", "denied"],
    ["denied", "approved"],
  ])("fails closed when same-sequence resolutions conflict: %s then %s", (first, second) => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "12",
      provisionalId: `approval-${first}`,
      itemId: approval.callId,
      toolName: approval.toolName,
      status: first,
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "12",
      provisionalId: `approval-${second}`,
      itemId: approval.callId,
      toolName: approval.toolName,
      status: second,
    }));

    expect(selectLatestPendingApproval(state)).toBeUndefined();
    expect([...state.approvalLifecycles.values()][0]).toMatchObject({
      runSequence: "12",
      phase: "closed",
    });
    expect(selectSemanticLiveItems(state)).toEqual([]);
  });

  it("keeps an identical same-sequence resolution replay idempotent", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    for (const provisionalId of ["approval-first", "approval-replay"]) {
      state = reduceLiveTimelineEvent(state, event({
        kind: "approval_resolved",
        runSequence: "12",
        provisionalId,
        itemId: approval.callId,
        toolName: approval.toolName,
        status: "approved",
      }));
    }

    expect([...state.approvalLifecycles.values()][0]).toMatchObject({
      runSequence: "12",
      phase: "resolved",
    });
    expect(selectSemanticLiveItems(state)).toHaveLength(1);
    expect(selectSemanticLiveItems(state)[0]?.status).toBe("approved");
  });

  it("filters delta replay covered by the live provisional anchor and prunes existing fragments", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "4", text: "old" }));
    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "6", text: "new" }));
    state = liveEventReducer(state, {
      type: "anchor_received",
      sessionId: SESSION_ID,
      anchor: { durableFrontier: "20", runId: "run-1", runSequence: "5" },
    });

    const buffer = selectDeltaBuffers(state)[0];
    expect(buffer === undefined ? "" : selectDeltaText(buffer)).toBe("new");
    state = reduceLiveTimelineEvent(state, event({ kind: "assistant_delta", runSequence: "5", text: "replay" }));
    expect(selectDeltaBuffers(state)[0]).toBe(buffer);

    state = reduceLiveTimelineEvent(state, event({
      runId: "run-2",
      kind: "assistant_delta",
      runSequence: "1",
      text: "other run",
    }));
    expect(selectDeltaBuffers(state)).toHaveLength(2);
  });

  it("ignores events from another session and treats current-session selection as a no-op", () => {
    const initial = createLiveEventState(SESSION_ID);
    const foreign = reduceLiveTimelineEvent(initial, event({
      sessionId: "session-2",
      kind: "assistant_delta",
      runSequence: "1",
      text: "foreign",
    }));
    expect(foreign).toBe(initial);
    expect(liveEventReducer(initial, {
      type: "session_selected",
      sessionId: SESSION_ID,
    })).toBe(initial);
  });
});

function event(overrides: Partial<TimelineEvent>): TimelineEvent {
  return {
    workspaceId: "workspace-1",
    sessionId: SESSION_ID,
    runId: "run-1",
    sequence: 0,
    runSequence: "0",
    replayable: false,
    kind: "other",
    ...overrides,
  };
}

function exactApproval(): TimelineApproval {
  return {
    callId: "call-1",
    toolName: "bash",
    approvalRequestId: "approval-1",
    toolCallHash: "hash-1",
    policyVersion: "policy-1",
    expiresAtMs: 4_102_444_800_000,
    snapshotRequired: false,
    previewTitle: "Review command",
  };
}
