import { describe, expect, it } from "vitest";

import type { ApprovalDecisionSummary, TimelineApproval, TimelineEvent } from "../../types";
import {
  createLiveEventState,
  liveEventReducer,
  reduceLiveTimelineEvent,
  selectDeltaBuffers,
  selectDeltaText,
  selectLatestApprovalPresentation,
  selectLatestPendingApproval,
  selectSemanticLiveItems,
  selectTaskEvents,
  selectTerminalTasks,
  selectTerminalSignals,
  semanticLiveItemFromTimelineEvent,
} from "./liveEventReducer";

const SESSION_ID = "session-1";

describe("live event reducer", () => {
  it("keeps bounded terminal task cards generation-monotonic across foreground settlement", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, terminalEvent("terminal-active", 1, "running", 10));
    state = liveEventReducer(state, {
      type: "run_discarded",
      sessionId: SESSION_ID,
      runId: "run-1",
    });
    state = reduceLiveTimelineEvent(state, terminalEvent("terminal-active", 1, "exited", 11));

    expect(selectTerminalTasks(state)).toMatchObject([
      { runId: "run-1", task: { taskId: "terminal-active", generation: 1, status: "running" } },
    ]);

    for (let index = 0; index < 8; index += 1) {
      state = reduceLiveTimelineEvent(
        state,
        terminalEvent(`terminal-done-${index}`, 1, "exited", 20 + index),
      );
    }
    expect(selectTerminalTasks(state)).toHaveLength(9);
    expect(selectTerminalTasks(state).some(({ task }) => task.taskId === "terminal-active")).toBe(true);

    state = reduceLiveTimelineEvent(state, terminalEvent("terminal-active", 2, "cancelled", 40));
    expect(selectTerminalTasks(state).find(({ task }) => task.taskId === "terminal-active")).toMatchObject({
      runId: "run-1",
      task: { generation: 2, status: "cancelled" },
    });
  });

  it("never evicts active terminals to satisfy the completed-history budget", () => {
    let state = createLiveEventState(SESSION_ID);
    for (let index = 0; index < 12; index += 1) {
      state = reduceLiveTimelineEvent(
        state,
        terminalEvent(`terminal-active-${index}`, 1, "running", 10 + index),
      );
    }
    for (let index = 0; index < 12; index += 1) {
      state = reduceLiveTimelineEvent(
        state,
        terminalEvent(`terminal-done-${index}`, 1, "exited", 100 + index),
      );
    }

    const retained = selectTerminalTasks(state);
    expect(retained.filter(({ task }) => task.status === "running")).toHaveLength(12);
    expect(retained.filter(({ task }) => task.status === "exited")).toHaveLength(8);
  });

  it("keeps reused task identifiers isolated by their owning run", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(
      state,
      terminalEvent("shared-task", 2, "exited", 10, "run-1"),
    );
    state = reduceLiveTimelineEvent(
      state,
      terminalEvent("shared-task", 1, "running", 20, "run-2"),
    );

    expect(selectTerminalTasks(state)).toMatchObject([
      { runId: "run-1", task: { taskId: "shared-task", generation: 2, status: "exited" } },
      { runId: "run-2", task: { taskId: "shared-task", generation: 1, status: "running" } },
    ]);
  });

  it("keeps typed task slots monotonic after canonical run settlement", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_step_changed",
      runSequence: "4",
      itemId: "step-1",
      status: "running",
      task: {
        taskId: "task-1",
        planVersion: 2,
        stepId: "step-1",
        attemptId: "attempt-1",
      },
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_step_changed",
      runSequence: "6",
      itemId: "step-1",
      status: "completed",
      task: {
        taskId: "task-1",
        planVersion: 2,
        stepId: "step-1",
        attemptId: "attempt-1",
      },
    }));
    const completed = state;
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_step_changed",
      runSequence: "5",
      itemId: "step-1",
      status: "running",
      task: {
        taskId: "task-1",
        planVersion: 2,
        stepId: "step-1",
        attemptId: "attempt-1",
      },
    }));

    expect(state).toBe(completed);
    expect(selectTaskEvents(state)).toMatchObject([
      {
        kind: "task_step_changed",
        runSequence: "6",
        status: "completed",
        task: { taskId: "task-1", planVersion: 2, stepId: "step-1" },
      },
    ]);

    state = liveEventReducer(state, {
      type: "run_discarded",
      sessionId: SESSION_ID,
      runId: "run-1",
    });
    expect(selectTaskEvents(state)).toMatchObject([
      {
        kind: "task_step_changed",
        status: "completed",
        task: { taskId: "task-1", stepId: "step-1" },
      },
    ]);
  });

  it("replaces terminal Task state when the same durable Task continues in a new run", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_run_started",
      runId: "run-1",
      runSequence: "1",
      task: { taskId: "task-1", objective: "Finish the task" },
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_run_finished",
      runId: "run-1",
      runSequence: "8",
      status: "interrupted",
      task: { taskId: "task-1" },
    }));
    state = liveEventReducer(state, {
      type: "run_discarded",
      sessionId: SESSION_ID,
      runId: "run-1",
    });
    state = reduceLiveTimelineEvent(state, event({
      kind: "task_run_started",
      runId: "run-2",
      runSequence: "1",
      task: { taskId: "task-1", objective: "Finish the task" },
    }));

    expect(selectTaskEvents(state).some((item) => item.kind === "task_run_finished")).toBe(false);
    expect(selectTaskEvents(state).find((item) => item.kind === "task_run_started")).toMatchObject({
      runId: "run-2",
      task: { taskId: "task-1" },
    });

    state = reduceLiveTimelineEvent(state, event({
      kind: "task_run_finished",
      runId: "run-1",
      runSequence: "9",
      status: "interrupted",
      task: { taskId: "task-1" },
    }));
    expect(selectTaskEvents(state).some((item) => item.kind === "task_run_finished")).toBe(false);
  });

  it("orders semantic items by exact decimal run sequence and ignores removed numeric sequence", () => {
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

  it("separates provider tool-call assembly from the execution lifecycle", () => {
    let state = createLiveEventState(SESSION_ID);
    const lifecycle = [
      event({
        runSequence: "1",
        provisionalId: "live-tool",
        kind: "tool_started",
        itemId: "call-1",
        toolName: "bash",
        toolInput: "head -1 README.md",
      }),
      event({
        runSequence: "2",
        provisionalId: "live-tool",
        kind: "tool_completed",
        itemId: "call-1",
        toolName: "bash",
      }),
      event({
        runSequence: "3",
        provisionalId: "live-tool",
        kind: "tool_progress",
        itemId: "call-1",
        toolName: "bash",
        status: "started",
      }),
      event({
        runSequence: "4",
        provisionalId: "live-tool",
        kind: "tool_progress",
        itemId: "call-1",
        toolName: "bash",
        status: "completed",
      }),
      event({
        runSequence: "5",
        provisionalId: "live-tool",
        kind: "tool_result",
        itemId: "call-1",
        toolName: "bash",
        status: "ok",
      }),
    ];

    const statuses = lifecycle.map((timelineEvent) => {
      state = reduceLiveTimelineEvent(state, timelineEvent);
      return selectSemanticLiveItems(state)[0]?.status;
    });

    expect(statuses).toEqual([
      "requested",
      "requested",
      "running",
      "completed",
      "succeeded",
    ]);
    expect(selectSemanticLiveItems(state)[0]?.toolInput).toBe("head -1 README.md");
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

  it("retains typed route recovery actions while terminating the unavailable run", () => {
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "route_recovery_required",
      runSequence: "10",
      status: "recovery_required",
      routeRecovery: {
        code: "session_route_confirmation_required",
        actions: ["repair_connection", "start_new_session", "back_to_session_library"],
        recoveryBinding: "route-binding-1",
        retryable: true,
      },
    }));

    expect(state.routeRecovery?.routeRecovery).toEqual({
      code: "session_route_confirmation_required",
      actions: ["repair_connection", "start_new_session", "back_to_session_library"],
      recoveryBinding: "route-binding-1",
      retryable: true,
    });
    expect(selectTerminalSignals(state)).toEqual([{
      runId: "run-1",
      runSequence: "10",
      status: "failed",
    }]);
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
      approvalRequestId: approval.approvalRequestId,
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

  it("turns only the exact accepted receipt into a non-actionable tombstone", () => {
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

    state = liveEventReducer(state, {
      type: "approval_receipt_received",
      sessionId: SESSION_ID,
      receipt: approvalReceipt({ approvalRequestId: "older-request" }),
    });
    expect(selectLatestPendingApproval(state)?.approval).toEqual(approval);

    state = liveEventReducer(state, {
      type: "approval_receipt_received",
      sessionId: SESSION_ID,
      receipt: approvalReceipt(),
    });
    expect(selectLatestPendingApproval(state)).toBeUndefined();
    expect(selectLatestApprovalPresentation(state)).toMatchObject({
      phase: "accepted",
      event: { approval },
    });
    expect([...state.approvalLifecycles.values()][0]).toMatchObject({
      approvalRequestId: approval.approvalRequestId,
      phase: "accepted",
      registryRevision: 11,
      decision: "approved",
    });
  });

  it("atomically rebuilds and clears pending approvals from a monotonic run snapshot", () => {
    const approval = exactApproval();
    const pending = event({
      kind: "approval_requested",
      runSequence: "10",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    });
    let state = createLiveEventState(SESSION_ID);
    state = liveEventReducer(state, {
      type: "approval_snapshot_received",
      sessionId: SESSION_ID,
      snapshot: {
        workspaceId: "workspace-1",
        sessionId: SESSION_ID,
        runId: "run-1",
        registryRevision: 12,
        pendingApprovals: [pending],
        approvalLifecycles: [],
      },
    });
    expect(selectLatestPendingApproval(state)?.approval).toEqual(approval);

    state = liveEventReducer(state, {
      type: "approval_snapshot_received",
      sessionId: SESSION_ID,
      snapshot: {
        workspaceId: "workspace-1",
        sessionId: SESSION_ID,
        runId: "run-1",
        registryRevision: 13,
        pendingApprovals: [],
        approvalLifecycles: [],
      },
    });
    expect(selectLatestPendingApproval(state)).toBeUndefined();

    state = liveEventReducer(state, {
      type: "approval_snapshot_received",
      sessionId: SESSION_ID,
      snapshot: {
        workspaceId: "workspace-1",
        sessionId: SESSION_ID,
        runId: "run-1",
        registryRevision: 12,
        pendingApprovals: [pending],
        approvalLifecycles: [],
      },
    });
    expect(selectLatestPendingApproval(state)).toBeUndefined();
  });

  it("recovers accepted approval identity and closes it only on exact typed execution", () => {
    const approval = exactApproval();
    const pending = event({
      kind: "approval_requested",
      runSequence: "10",
      itemId: approval.callId,
      toolName: approval.toolName,
      approval,
    });
    let state = liveEventReducer(createLiveEventState(SESSION_ID), {
      type: "approval_snapshot_received",
      sessionId: SESSION_ID,
      snapshot: {
        workspaceId: "workspace-1",
        sessionId: SESSION_ID,
        runId: "run-1",
        registryRevision: 13,
        pendingApprovals: [],
        approvalLifecycles: [{ event: pending, state: "decision_accepted" }],
      },
    });

    expect(selectLatestApprovalPresentation(state)).toMatchObject({
      phase: "accepted",
      event: { approval },
    });

    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      runSequence: "14",
      itemId: approval.callId,
      approvalRequestId: "approval-other",
      status: "approved",
    }));
    expect([...state.approvalLifecycles.values()][0]?.phase).toBe("accepted");

    state = reduceLiveTimelineEvent(state, event({
      kind: "control",
      runSequence: "15",
      itemId: "tool_execution",
      toolExecution: {
        callId: approval.callId,
        toolName: approval.toolName,
        status: "started",
      },
    }));
    expect(selectLatestApprovalPresentation(state)).toBeUndefined();
    expect([...state.approvalLifecycles.values()][0]?.phase).toBe("execution_started");
  });

  it("does not let an old receipt close a newer request for the same call", () => {
    const oldApproval = exactApproval();
    const newApproval = { ...oldApproval, approvalRequestId: "approval-2" };
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "10",
      itemId: oldApproval.callId,
      approval: oldApproval,
    }));
    state = liveEventReducer(state, {
      type: "approval_receipt_received",
      sessionId: SESSION_ID,
      receipt: approvalReceipt(),
    });
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_requested",
      runSequence: "12",
      itemId: newApproval.callId,
      approval: newApproval,
    }));
    state = liveEventReducer(state, {
      type: "approval_receipt_received",
      sessionId: SESSION_ID,
      receipt: approvalReceipt({ routeState: "terminal", registryRevision: 99 }),
    });

    expect(selectLatestPendingApproval(state)?.approval).toEqual(newApproval);
    expect(selectLatestApprovalPresentation(state)?.phase).toBe("pending");
  });

  it("keeps a resolved high-water tombstone so an older request cannot revive", () => {
    const approval = exactApproval();
    let state = createLiveEventState(SESSION_ID);
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      approvalRequestId: approval.approvalRequestId,
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
      approvalRequestId: approval.approvalRequestId,
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
      approvalRequestId: approval.approvalRequestId,
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
      approvalRequestId: approval.approvalRequestId,
      runSequence: "12",
      provisionalId: `approval-${first}`,
      itemId: approval.callId,
      toolName: approval.toolName,
      status: first,
    }));
    state = reduceLiveTimelineEvent(state, event({
      kind: "approval_resolved",
      approvalRequestId: approval.approvalRequestId,
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
        approvalRequestId: approval.approvalRequestId,
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

function terminalEvent(
  taskId: string,
  generation: number,
  status: "running" | "exited" | "cancelled",
  emittedAtMs: number,
  runId = "run-1",
): TimelineEvent {
  return event({
    runId,
    kind: "terminal_lifecycle",
    runSequence: String(emittedAtMs),
    itemId: taskId,
    status,
    terminalTask: {
      taskId,
      generation,
      status,
      exitCode: status === "exited" ? 0 : undefined,
      readiness: "ready",
      readinessKind: "output_contains",
      readyAtMs: 5,
      totalOutputBytes: 16,
      emittedAtMs,
    },
  });
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

function approvalReceipt(
  overrides: Partial<ApprovalDecisionSummary> = {},
): ApprovalDecisionSummary {
  return {
    commandId: "approval-command-1",
    clientId: "desktop-test",
    sessionId: SESSION_ID,
    runId: "run-1",
    callId: "call-1",
    approvalRequestId: "approval-1",
    expectedStreamSequence: 10,
    decision: "approved",
    routeState: "decision_accepted",
    registryRevision: 11,
    replayed: false,
    ...overrides,
  };
}
