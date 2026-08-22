import type {
  ApprovalDecisionSummary,
  RunApprovalSnapshot,
  TimelineEvent,
  TimelineTerminalTask,
} from "../../types";
import type {
  ConversationTerminalStatus,
  LiveConversationDisplayItem,
} from "./continuityReducer";

export type LiveDeltaChannel = "assistant" | "reasoning";

export interface LiveProvisionalAnchor {
  durableFrontier: string;
  runId: string;
  runSequence: string;
}

export interface LiveDeltaBuffer {
  identity: string;
  runId: string;
  channel: LiveDeltaChannel;
  firstRunSequence: string;
  lastRunSequence: string;
  fragments: ReadonlyMap<string, string>;
}

export interface LiveTerminalSignal {
  runId: string;
  runSequence: string;
  status: ConversationTerminalStatus;
}

export interface LiveTerminalTask {
  runId: string;
  task: TimelineTerminalTask;
}

export type ApprovalLifecyclePhase =
  | "pending"
  | "resolving"
  | "accepted"
  | "delivery_uncertain"
  | "resolved"
  | "execution_started"
  | "terminal"
  | "closed";

export interface ApprovalLifecycle {
  runId: string;
  callId: string;
  approvalRequestId?: string;
  runSequence: string;
  phase: ApprovalLifecyclePhase;
  registryRevision?: number;
  decision?: ApprovalDecisionSummary["decision"];
  /** Exact semantic result retained by a resolved tombstone for replay validation. */
  resolutionFingerprint?: string;
}

export interface LiveEventState {
  sessionId: string;
  anchor?: LiveProvisionalAnchor;
  semanticItems: ReadonlyMap<string, LiveConversationDisplayItem>;
  deltaBuffers: ReadonlyMap<string, LiveDeltaBuffer>;
  /** Pending approval events containing the exact guard required by the control route. */
  controlEvents: ReadonlyMap<string, TimelineEvent>;
  /** Exact per-call high-water marks, including resolved and fail-closed tombstones. */
  approvalLifecycles: ReadonlyMap<string, ApprovalLifecycle>;
  /** Registry revision of the latest canonical approval snapshot for each run. */
  approvalSnapshotRevisions: ReadonlyMap<string, number>;
  /** Latest typed task event for each exact task/entity slot. */
  taskEvents: ReadonlyMap<string, TimelineEvent>;
  /** Latest generation for each bounded persistent terminal task. */
  terminalTasks: ReadonlyMap<string, LiveTerminalTask>;
  /** Latest root Task selected by a durable task-start event. */
  latestTaskId?: string;
  /** Run currently allowed to advance the selected durable Task. */
  latestTaskRunId?: string;
  terminalSignals: ReadonlyMap<string, LiveTerminalSignal>;
  routeRecovery?: TimelineEvent;
  /** Most recent provider-turn recovery for the active run, projected without attempt identity. */
  providerTurnRecovery?: TimelineEvent;
}

export type LiveEventAction =
  | { type: "session_selected"; sessionId: string }
  | { type: "anchor_received"; sessionId: string; anchor?: LiveProvisionalAnchor }
  | { type: "event_received"; sessionId: string; event: TimelineEvent }
  | { type: "approval_snapshot_received"; sessionId: string; snapshot: RunApprovalSnapshot }
  | { type: "approval_receipt_received"; sessionId: string; receipt: ApprovalDecisionSummary }
  | {
      type: "terminal_task_received";
      sessionId: string;
      runId: string;
      task: TimelineTerminalTask;
    }
  | { type: "run_discarded"; sessionId: string; runId: string };

export function createLiveEventState(sessionId: string): LiveEventState {
  return {
    sessionId,
    semanticItems: new Map(),
    deltaBuffers: new Map(),
    controlEvents: new Map(),
    approvalLifecycles: new Map(),
    approvalSnapshotRevisions: new Map(),
    taskEvents: new Map(),
    terminalTasks: new Map(),
    terminalSignals: new Map(),
  };
}

export function liveEventReducer(
  state: LiveEventState,
  action: LiveEventAction,
): LiveEventState {
  if (action.type === "session_selected") {
    return action.sessionId === state.sessionId
      ? state
      : createLiveEventState(action.sessionId);
  }
  if (action.sessionId !== state.sessionId) return state;

  switch (action.type) {
    case "anchor_received":
      return receiveAnchor(state, action.anchor);
    case "event_received":
      return receiveTimelineEvent(state, action.event);
    case "approval_snapshot_received":
      return receiveApprovalSnapshot(state, action.snapshot);
    case "approval_receipt_received":
      return receiveApprovalReceipt(state, action.receipt);
    case "terminal_task_received":
      return receiveTerminalTask(state, action.runId, action.task);
    case "run_discarded":
      return discardRun(state, action.runId);
  }
}

function receiveApprovalSnapshot(
  state: LiveEventState,
  snapshot: RunApprovalSnapshot,
): LiveEventState {
  const priorRevision = state.approvalSnapshotRevisions.get(snapshot.runId) ?? 0;
  if (snapshot.registryRevision < priorRevision) return state;

  const canonical = new Map<string, {
    event: TimelineEvent;
    phase: ApprovalLifecyclePhase;
  }>();
  for (const event of snapshot.pendingApprovals) {
    const callId = event.approval?.callId ?? event.itemId;
    if (
      event.sessionId !== state.sessionId
      || event.runId !== snapshot.runId
      || event.kind !== "approval_requested"
      || event.approval === undefined
      || callId === undefined
    ) continue;
    canonical.set(approvalLifecycleKey(snapshot.runId, callId), { event, phase: "pending" });
  }
  for (const lifecycle of snapshot.approvalLifecycles) {
    const event = lifecycle.event;
    const callId = event.approval?.callId ?? event.itemId;
    if (
      event.sessionId !== state.sessionId
      || event.runId !== snapshot.runId
      || event.kind !== "approval_requested"
      || event.approval === undefined
      || callId === undefined
    ) continue;
    canonical.set(approvalLifecycleKey(snapshot.runId, callId), {
      event,
      phase: approvalSnapshotPhase(lifecycle.state),
    });
  }

  const controlEvents = new Map(state.controlEvents);
  const approvalLifecycles = new Map(state.approvalLifecycles);
  for (const [key, lifecycle] of approvalLifecycles) {
    if (lifecycle.runId !== snapshot.runId || canonical.has(key)) continue;
    if (
      lifecycle.phase === "pending"
      || lifecycle.phase === "resolving"
      || lifecycle.phase === "accepted"
      || lifecycle.phase === "resolved"
      || lifecycle.phase === "delivery_uncertain"
      || lifecycle.phase === "execution_started"
    ) {
      approvalLifecycles.set(key, { ...lifecycle, phase: "closed" });
      controlEvents.delete(key);
    }
  }
  for (const [key, { event, phase }] of canonical) {
    const callId = event.approval?.callId ?? event.itemId;
    if (callId === undefined) continue;
    if (approvalPhaseIsPresentable(phase)) {
      controlEvents.set(key, event);
    } else {
      controlEvents.delete(key);
    }
    approvalLifecycles.set(key, {
      runId: snapshot.runId,
      callId,
      approvalRequestId: event.approval?.approvalRequestId,
      runSequence: String(snapshot.registryRevision),
      phase,
      registryRevision: snapshot.registryRevision,
    });
  }
  const approvalSnapshotRevisions = new Map(state.approvalSnapshotRevisions);
  approvalSnapshotRevisions.set(snapshot.runId, snapshot.registryRevision);
  return {
    ...state,
    controlEvents,
    approvalLifecycles,
    approvalSnapshotRevisions,
  };
}

function approvalSnapshotPhase(
  state: RunApprovalSnapshot["approvalLifecycles"][number]["state"],
): ApprovalLifecyclePhase {
  switch (state) {
    case "resolving": return "resolving";
    case "decision_accepted": return "accepted";
    case "resolved": return "resolved";
    case "execution_started": return "execution_started";
    case "delivery_uncertain": return "delivery_uncertain";
    case "terminal": return "terminal";
  }
}

function approvalPhaseIsPresentable(phase: ApprovalLifecyclePhase): boolean {
  return phase === "pending"
    || phase === "resolving"
    || phase === "accepted"
    || phase === "resolved"
    || phase === "delivery_uncertain";
}

export function reduceLiveTimelineEvent(
  state: LiveEventState,
  event: TimelineEvent,
): LiveEventState {
  return liveEventReducer(state, {
    type: "event_received",
    sessionId: event.sessionId,
    event,
  });
}

export function selectSemanticLiveItems(state: LiveEventState): LiveConversationDisplayItem[] {
  return [...state.semanticItems.values()].sort(compareSemanticItems);
}

export function selectDeltaBuffers(state: LiveEventState): LiveDeltaBuffer[] {
  return [...state.deltaBuffers.values()].sort(compareDeltaBuffers);
}

export function selectDeltaText(buffer: LiveDeltaBuffer): string {
  return [...buffer.fragments.entries()]
    .sort(([left], [right]) => compareRunSequence(left, right))
    .map(([, text]) => text)
    .join("");
}

export function selectPendingApprovalEvents(state: LiveEventState): TimelineEvent[] {
  return [...state.controlEvents.entries()]
    .filter(([key]) => state.approvalLifecycles.get(key)?.phase === "pending")
    .map(([, event]) => event)
    .sort(compareTimelineEvents);
}

export function selectLatestPendingApproval(state: LiveEventState): TimelineEvent | undefined {
  const pending = selectPendingApprovalEvents(state);
  return pending[pending.length - 1];
}

export interface ApprovalPresentation {
  event: TimelineEvent;
  phase: "pending" | "accepted" | "delivery_uncertain";
}

export function selectLatestApprovalPresentation(
  state: LiveEventState,
): ApprovalPresentation | undefined {
  const presentations = [...state.controlEvents.entries()]
    .flatMap(([key, event]) => {
      const phase = state.approvalLifecycles.get(key)?.phase;
      const presentationPhase = phase === "resolving" || phase === "resolved"
        ? "accepted"
        : phase;
      return presentationPhase === "pending"
        || presentationPhase === "accepted"
        || presentationPhase === "delivery_uncertain"
        ? [{ event, phase: presentationPhase }]
        : [];
    })
    .sort((left, right) => compareTimelineEvents(left.event, right.event));
  const pending = presentations.filter((presentation) => presentation.phase === "pending");
  return pending[pending.length - 1] ?? presentations[presentations.length - 1];
}

export function selectTerminalSignals(state: LiveEventState): LiveTerminalSignal[] {
  return [...state.terminalSignals.values()].sort((left, right) => {
    const sequence = compareRunSequence(left.runSequence, right.runSequence);
    return sequence !== 0 ? sequence : left.runId.localeCompare(right.runId);
  });
}

export function selectTaskEvents(state: LiveEventState): TimelineEvent[] {
  return [...state.taskEvents.values()]
    .filter((event) => (
      state.latestTaskId === undefined
      || event.task?.taskId === undefined
      || event.task.taskId === state.latestTaskId
    ))
    .sort(compareTimelineEvents);
}

export function selectTerminalTasks(state: LiveEventState): LiveTerminalTask[] {
  return [...state.terminalTasks.values()].sort((left, right) => {
    const emitted = left.task.emittedAtMs - right.task.emittedAtMs;
    return emitted !== 0 ? emitted : left.task.taskId.localeCompare(right.task.taskId);
  });
}

export function selectProviderTurnRecovery(state: LiveEventState): TimelineEvent | undefined {
  return state.providerTurnRecovery;
}

export function semanticLiveItemFromTimelineEvent(
  event: TimelineEvent,
): LiveConversationDisplayItem | undefined {
  const provisionalId = event.provisionalId;
  if (provisionalId === undefined || !isDecimalSequence(event.runSequence)) return undefined;

  switch (event.kind) {
    case "run_started":
      return {
        provisionalId,
        runId: event.runId,
        runSequence: event.runSequence,
        kind: "user_message",
        status: "running",
        content: { type: "message", role: "user", text: event.text },
      };
    case "assistant_message":
      if (event.assistantKind === "reasoning_trace") {
        return {
          provisionalId,
          runId: event.runId,
          runSequence: event.runSequence,
          kind: "reasoning",
          status: "streaming",
          content: { type: "reasoning", text: event.text ?? "" },
        };
      }
      return {
        provisionalId,
        runId: event.runId,
        runSequence: event.runSequence,
        kind: "assistant_message",
        status: "streaming",
        content: {
          type: "message",
          role: "assistant",
          text: event.text,
          assistantPhase: event.assistantKind,
        },
      };
    case "tool_started":
    case "tool_completed":
    case "tool_progress":
    case "tool_result":
      return {
        provisionalId,
        runId: event.runId,
        runSequence: event.runSequence,
        kind: "tool",
        status: toolStatus(event),
        content: {
          type: "tool",
          callId: event.itemId,
          toolName: event.toolName,
          output: event.text,
        },
        toolInput: event.toolInput,
      };
    case "approval_requested":
    case "approval_resolved": {
      const callId = event.approval?.callId ?? event.itemId;
      const toolName = event.approval?.toolName ?? event.toolName;
      if (callId === undefined || toolName === undefined) return undefined;
      const approved = event.status === "approved" || event.status === "approved_for_session";
      return {
        provisionalId,
        runId: event.runId,
        runSequence: event.runSequence,
        kind: "approval",
        status: event.kind === "approval_requested"
          ? "waiting_for_approval"
          : approved ? "approved" : "denied",
        content: {
          type: "approval",
          callId,
          toolName,
          decision: event.kind === "approval_requested"
            ? undefined
            : event.status === "approved_for_session" ? "approved_for_session"
            : approved ? "approved" : "denied",
        },
      };
    }
    default:
      return undefined;
  }
}

export function terminalSignalFromTimelineEvent(
  event: TimelineEvent,
): LiveTerminalSignal | undefined {
  if (!isDecimalSequence(event.runSequence)) return undefined;
  const status = terminalStatus(event);
  return status === undefined
    ? undefined
    : { runId: event.runId, runSequence: event.runSequence, status };
}

export function compareRunSequence(left: string, right: string): number {
  const leftValue = BigInt(left);
  const rightValue = BigInt(right);
  return leftValue < rightValue ? -1 : leftValue > rightValue ? 1 : 0;
}

function receiveAnchor(
  state: LiveEventState,
  anchor: LiveProvisionalAnchor | undefined,
): LiveEventState {
  if (anchor !== undefined && !isDecimalSequence(anchor.runSequence)) return state;
  if (sameAnchor(state.anchor, anchor)) return state;

  const deltaBuffers = new Map(state.deltaBuffers);
  if (anchor !== undefined) {
    for (const [key, buffer] of deltaBuffers) {
      if (buffer.runId !== anchor.runId) continue;
      const fragments = new Map(
        [...buffer.fragments].filter(([sequence]) => (
          compareRunSequence(sequence, anchor.runSequence) > 0
        )),
      );
      if (fragments.size === 0) {
        deltaBuffers.delete(key);
      } else {
        deltaBuffers.set(key, rebuildDeltaBuffer(buffer.runId, buffer.channel, fragments));
      }
    }
  }
  return { ...state, anchor, deltaBuffers };
}

function receiveTimelineEvent(state: LiveEventState, event: TimelineEvent): LiveEventState {
  if (event.sessionId !== state.sessionId || !isDecimalSequence(event.runSequence)) return state;

  const controlUpdate = updateControlEvents(state, event);
  let next = updateTerminalTasks(updateTaskEvents(controlUpdate.state, event), event);
  if (event.kind === "route_recovery_required" && event.routeRecovery !== undefined) {
    next = { ...next, routeRecovery: event };
  } else if (event.kind === "run_started" && next.routeRecovery !== undefined) {
    next = { ...next, routeRecovery: undefined };
  }
  if (event.kind === "provider_turn_recovery" && event.providerTurnRecovery !== undefined) {
    next = { ...next, providerTurnRecovery: event };
  } else if (event.kind === "run_started" && next.providerTurnRecovery !== undefined) {
    next = { ...next, providerTurnRecovery: undefined };
  }
  const terminalSignal = terminalSignalFromTimelineEvent(event);
  if (terminalSignal !== undefined) {
    return receiveTerminalSignal(next, terminalSignal);
  }

  if (event.kind === "assistant_delta" || event.kind === "reasoning_delta") {
    return receiveDelta(next, event);
  }

  if (event.kind === "assistant_message") {
    next = clearRunDeltaChannelThrough(
      next,
      event.runId,
      event.assistantKind === "reasoning_trace" ? "reasoning" : "assistant",
      event.runSequence,
    );
  } else if (isToolBoundary(event.kind)) {
    next = clearRunDeltaChannelThrough(next, event.runId, "assistant", event.runSequence);
  } else if (event.kind === "provider_turn_partial_output_discarded") {
    next = clearRunDeltaBuffersThrough(next, event.runId, event.runSequence);
  } else if (event.kind === "run_started") {
    next = clearRunDeltaBuffersThrough(next, event.runId, event.runSequence);
  }

  const item = semanticLiveItemFromTimelineEvent(event);
  return item === undefined || !controlUpdate.applySemantic
    ? next
    : receiveSemanticItem(next, item);
}

const MAX_TERMINAL_TASK_HISTORY_CARDS = 8;

function updateTerminalTasks(state: LiveEventState, event: TimelineEvent): LiveEventState {
  if (event.kind !== "terminal_lifecycle") return state;
  const task = event.terminalTask;
  if (task === undefined || event.itemId !== task.taskId) return state;
  return receiveTerminalTask(state, event.runId, task);
}

function receiveTerminalTask(
  state: LiveEventState,
  runId: string,
  task: TimelineTerminalTask,
): LiveEventState {
  const key = terminalTaskKey(runId, task.taskId);
  const current = state.terminalTasks.get(key);
  if (current !== undefined && current.task.generation >= task.generation) return state;

  const terminalTasks = new Map(state.terminalTasks);
  terminalTasks.set(key, { runId, task });
  while (
    [...terminalTasks.values()].filter(({ task: currentTask }) => (
      !terminalTaskIsActive(currentTask)
    )).length > MAX_TERMINAL_TASK_HISTORY_CARDS
  ) {
    const candidate = [...terminalTasks.values()]
      .filter(({ task: currentTask }) => !terminalTaskIsActive(currentTask))
      .sort((left, right) => left.task.emittedAtMs - right.task.emittedAtMs)[0];
    if (candidate === undefined) break;
    terminalTasks.delete(terminalTaskKey(candidate.runId, candidate.task.taskId));
  }
  return { ...state, terminalTasks };
}

function terminalTaskKey(runId: string, taskId: string): string {
  return `${runId}\u0000${taskId}`;
}

function terminalTaskIsActive(task: TimelineTerminalTask): boolean {
  return task.status === "starting" || task.status === "running";
}

function updateTaskEvents(state: LiveEventState, event: TimelineEvent): LiveEventState {
  const key = taskEventKey(event);
  if (key === undefined) return state;
  const taskId = event.task?.taskId;
  if (
    event.kind !== "task_run_started"
    && taskId !== undefined
    && state.latestTaskId !== undefined
    && (
      taskId !== state.latestTaskId
      || (
        state.latestTaskRunId !== undefined
        && event.runId !== state.latestTaskRunId
      )
    )
  ) return state;
  const startsNewTask = event.kind === "task_run_started"
    && taskId !== undefined
    && taskId !== state.latestTaskId;
  const taskEvents = startsNewTask ? new Map<string, TimelineEvent>() : new Map(state.taskEvents);
  if (event.kind === "task_run_started" && taskId !== undefined) {
    taskEvents.delete(`${taskId}:task_run_finished:task`);
    taskEvents.delete(`${taskId}:task_phase_changed:task`);
  }
  const existing = taskEvents.get(key);
  if (
    !startsNewTask
    && existing !== undefined
    && existing.runId === event.runId
    && compareRunSequence(event.runSequence, existing.runSequence) <= 0
  ) return state;
  taskEvents.set(key, event);
  return {
    ...state,
    taskEvents,
    latestTaskId: event.kind === "task_run_started" && taskId !== undefined
      ? taskId
      : state.latestTaskId ?? taskId,
    latestTaskRunId: event.kind === "task_run_started" && taskId !== undefined
      ? event.runId
      : state.latestTaskRunId,
  };
}

function taskEventKey(event: TimelineEvent): string | undefined {
  const task = event.task;
  if (task === undefined) return undefined;
  const taskIdentity = task.taskId ?? (
    task.handoffId === undefined ? undefined : `handoff:${task.handoffId}`
  );
  if (taskIdentity === undefined) return undefined;
  const entityIdentity = task.stepId
    ?? task.laneId
    ?? task.batchId
    ?? task.handoffId
    ?? "task";
  return `${taskIdentity}:${event.kind}:${entityIdentity}`;
}

function receiveSemanticItem(
  state: LiveEventState,
  item: LiveConversationDisplayItem,
): LiveEventState {
  const existing = state.semanticItems.get(item.provisionalId);
  if (existing !== undefined && compareRunSequence(item.runSequence, existing.runSequence) <= 0) {
    return state;
  }
  const semanticItems = new Map(state.semanticItems);
  semanticItems.set(
    item.provisionalId,
    item.toolInput === undefined && existing?.toolInput !== undefined
      ? { ...item, toolInput: existing.toolInput }
      : item,
  );
  return { ...state, semanticItems };
}

function receiveDelta(state: LiveEventState, event: TimelineEvent): LiveEventState {
  if (event.provisionalId !== undefined || deltaIsCoveredByAnchor(state.anchor, event)) return state;
  const channel: LiveDeltaChannel = event.kind === "assistant_delta" ? "assistant" : "reasoning";
  const key = deltaBufferKey(event.runId, channel);
  const current = state.deltaBuffers.get(key);
  if (current?.fragments.has(event.runSequence) === true) return state;

  const fragments = new Map(current?.fragments ?? []);
  fragments.set(event.runSequence, event.text ?? "");
  const deltaBuffers = new Map(state.deltaBuffers);
  deltaBuffers.set(key, rebuildDeltaBuffer(event.runId, channel, fragments));
  return { ...state, deltaBuffers };
}

function receiveTerminalSignal(
  state: LiveEventState,
  signal: LiveTerminalSignal,
): LiveEventState {
  const current = state.terminalSignals.get(signal.runId);
  if (current !== undefined && compareRunSequence(signal.runSequence, current.runSequence) <= 0) {
    return state;
  }
  const terminalSignals = new Map(state.terminalSignals);
  terminalSignals.set(signal.runId, signal);
  return { ...state, terminalSignals };
}

function receiveApprovalReceipt(
  state: LiveEventState,
  receipt: ApprovalDecisionSummary,
): LiveEventState {
  const key = approvalLifecycleKey(receipt.runId, receipt.callId);
  const current = state.approvalLifecycles.get(key);
  const pendingEvent = state.controlEvents.get(key);
  if (
    current === undefined
    || pendingEvent?.approval === undefined
    || current.approvalRequestId !== receipt.approvalRequestId
    || pendingEvent.approval.approvalRequestId !== receipt.approvalRequestId
    || current.phase === "resolved"
    || current.phase === "execution_started"
    || current.phase === "terminal"
    || current.phase === "closed"
    || (
      current.registryRevision !== undefined
      && receipt.registryRevision < current.registryRevision
    )
  ) return state;

  if (receipt.routeState === "terminal") {
    return replaceApprovalLifecycle(state, key, {
      ...current,
      phase: "closed",
      registryRevision: receipt.registryRevision,
      decision: receipt.decision,
    });
  }
  return replaceApprovalLifecycle(state, key, {
    ...current,
    phase: receipt.routeState === "delivery_uncertain" ? "delivery_uncertain" : "accepted",
    registryRevision: receipt.registryRevision,
    decision: receipt.decision,
  }, pendingEvent);
}

interface ApprovalControlUpdate {
  state: LiveEventState;
  applySemantic: boolean;
}

function updateControlEvents(
  state: LiveEventState,
  event: TimelineEvent,
): ApprovalControlUpdate {
  if (event.kind === "control" && event.toolExecution !== undefined) {
    return updateToolExecutionControl(state, event);
  }
  if (event.kind !== "approval_requested" && event.kind !== "approval_resolved") {
    return { state, applySemantic: true };
  }
  const callId = event.approval?.callId ?? event.itemId;
  if (callId === undefined) return { state, applySemantic: false };
  const key = approvalLifecycleKey(event.runId, callId);
  const current = state.approvalLifecycles.get(key);
  if (
    event.kind === "approval_resolved"
    && current?.approvalRequestId !== undefined
    && current.approvalRequestId !== event.approvalRequestId
  ) {
    return { state, applySemantic: false };
  }
  if (current !== undefined) {
    const sequence = compareRunSequence(event.runSequence, current.runSequence);
    if (sequence < 0) return { state, applySemantic: false };
    if (sequence === 0) return updateSameSequenceApproval(state, event, key, current);
  }

  if (event.kind === "approval_requested" && event.approval === undefined) {
    return {
      state: replaceApprovalLifecycle(state, key, {
        runId: event.runId,
        callId,
        runSequence: event.runSequence,
        phase: "closed",
      }),
      applySemantic: false,
    };
  }

  const phase: ApprovalLifecyclePhase = event.kind === "approval_requested"
    ? "pending"
    : "resolved";
  return {
    state: replaceApprovalLifecycle(state, key, {
      runId: event.runId,
      callId,
      approvalRequestId: event.approval?.approvalRequestId ?? event.approvalRequestId,
      runSequence: event.runSequence,
      phase,
      resolutionFingerprint: event.kind === "approval_resolved"
        ? approvalResolutionFingerprint(event)
        : undefined,
    }, phase === "pending" ? event : undefined),
    applySemantic: true,
  };
}

function updateToolExecutionControl(
  state: LiveEventState,
  event: TimelineEvent,
): ApprovalControlUpdate {
  const execution = event.toolExecution;
  if (execution === undefined) return { state, applySemantic: false };
  const key = approvalLifecycleKey(event.runId, execution.callId);
  const current = state.approvalLifecycles.get(key);
  if (current === undefined) return { state, applySemantic: true };
  if (compareRunSequence(event.runSequence, current.runSequence) < 0) {
    return { state, applySemantic: false };
  }
  const phase: ApprovalLifecyclePhase = execution.status === "started"
    ? "execution_started"
    : "terminal";
  return {
    state: replaceApprovalLifecycle(state, key, {
      ...current,
      runSequence: event.runSequence,
      phase,
    }),
    applySemantic: true,
  };
}

function updateSameSequenceApproval(
  state: LiveEventState,
  event: TimelineEvent,
  key: string,
  current: ApprovalLifecycle,
): ApprovalControlUpdate {
  if (current.phase === "closed") {
    return { state, applySemantic: false };
  }

  if (current.phase === "resolved") {
    if (event.kind === "approval_requested") {
      return { state, applySemantic: false };
    }
    if (
      current.resolutionFingerprint === approvalResolutionFingerprint(event)
    ) {
      return { state, applySemantic: false };
    }
    return {
      state: replaceApprovalLifecycle(state, key, {
        ...current,
        phase: "closed",
        resolutionFingerprint: undefined,
      }),
      applySemantic: false,
    };
  }

  if (event.kind === "approval_resolved") {
    if (
      current.approvalRequestId !== undefined
      && current.approvalRequestId !== event.approvalRequestId
    ) {
      return { state, applySemantic: false };
    }
    return {
      state: replaceApprovalLifecycle(state, key, {
        ...current,
        phase: "resolved",
        resolutionFingerprint: approvalResolutionFingerprint(event),
      }),
      applySemantic: true,
    };
  }

  const pending = state.controlEvents.get(key);
  if (
    event.approval !== undefined
    && pending?.approval !== undefined
    && sameApprovalGuard(event.approval, pending.approval)
  ) {
    return { state, applySemantic: false };
  }

  return {
    state: replaceApprovalLifecycle(state, key, { ...current, phase: "closed" }),
    applySemantic: false,
  };
}

function approvalResolutionFingerprint(event: TimelineEvent): string {
  return JSON.stringify({
    status: event.status ?? null,
    toolName: event.approval?.toolName ?? event.toolName ?? null,
    approvalRequestId: event.approvalRequestId ?? null,
  });
}

function replaceApprovalLifecycle(
  state: LiveEventState,
  key: string,
  lifecycle: ApprovalLifecycle,
  pendingEvent?: TimelineEvent,
): LiveEventState {
  const controlEvents = new Map(state.controlEvents);
  if (pendingEvent === undefined) {
    controlEvents.delete(key);
  } else {
    controlEvents.set(key, pendingEvent);
  }
  const approvalLifecycles = new Map(state.approvalLifecycles);
  approvalLifecycles.set(key, lifecycle);
  const semanticItems = filterMap(state.semanticItems, (item) => !(
    item.runId === lifecycle.runId
    && item.kind === "approval"
    && item.content.type === "approval"
    && item.content.callId === lifecycle.callId
  ));
  return { ...state, semanticItems, controlEvents, approvalLifecycles };
}

function discardRun(state: LiveEventState, runId: string): LiveEventState {
  const semanticItems = filterMap(state.semanticItems, (item) => item.runId !== runId);
  const deltaBuffers = filterMap(state.deltaBuffers, (buffer) => buffer.runId !== runId);
  const controlEvents = filterMap(state.controlEvents, (event) => event.runId !== runId);
  const approvalLifecycles = filterMap(
    state.approvalLifecycles,
    (lifecycle) => lifecycle.runId !== runId,
  );
  const terminalSignals = new Map(state.terminalSignals);
  terminalSignals.delete(runId);
  if (
    semanticItems.size === state.semanticItems.size
    && deltaBuffers.size === state.deltaBuffers.size
    && controlEvents.size === state.controlEvents.size
    && approvalLifecycles.size === state.approvalLifecycles.size
    && terminalSignals.size === state.terminalSignals.size
  ) return state;
  return {
    ...state,
    semanticItems,
    deltaBuffers,
    controlEvents,
    approvalLifecycles,
    terminalSignals,
  };
}

function clearRunDeltaBuffersThrough(
  state: LiveEventState,
  runId: string,
  throughRunSequence: string,
): LiveEventState {
  let changed = false;
  const deltaBuffers = new Map(state.deltaBuffers);
  for (const [key, buffer] of deltaBuffers) {
    if (buffer.runId !== runId) continue;
    const fragments = new Map(
      [...buffer.fragments].filter(([sequence]) => (
        compareRunSequence(sequence, throughRunSequence) > 0
      )),
    );
    if (fragments.size === buffer.fragments.size) continue;
    changed = true;
    if (fragments.size === 0) {
      deltaBuffers.delete(key);
    } else {
      deltaBuffers.set(key, rebuildDeltaBuffer(buffer.runId, buffer.channel, fragments));
    }
  }
  return changed ? { ...state, deltaBuffers } : state;
}

function clearRunDeltaChannelThrough(
  state: LiveEventState,
  runId: string,
  channel: LiveDeltaChannel,
  throughRunSequence: string,
): LiveEventState {
  const key = deltaBufferKey(runId, channel);
  const buffer = state.deltaBuffers.get(key);
  if (buffer === undefined) return state;
  const fragments = new Map(
    [...buffer.fragments].filter(([sequence]) => (
      compareRunSequence(sequence, throughRunSequence) > 0
    )),
  );
  if (fragments.size === buffer.fragments.size) return state;
  const deltaBuffers = new Map(state.deltaBuffers);
  if (fragments.size === 0) {
    deltaBuffers.delete(key);
  } else {
    deltaBuffers.set(key, rebuildDeltaBuffer(runId, channel, fragments));
  }
  return { ...state, deltaBuffers };
}

function filterMap<K, V>(
  source: ReadonlyMap<K, V>,
  predicate: (value: V) => boolean,
): Map<K, V> {
  return new Map([...source].filter(([, value]) => predicate(value)));
}

function rebuildDeltaBuffer(
  runId: string,
  channel: LiveDeltaChannel,
  fragments: ReadonlyMap<string, string>,
): LiveDeltaBuffer {
  const sequences = [...fragments.keys()].sort(compareRunSequence);
  const firstRunSequence = sequences[0] ?? "0";
  return {
    identity: `ephemeral:${runId}:${channel}:${firstRunSequence}`,
    runId,
    channel,
    firstRunSequence,
    lastRunSequence: sequences[sequences.length - 1] ?? firstRunSequence,
    fragments,
  };
}

function deltaIsCoveredByAnchor(
  anchor: LiveProvisionalAnchor | undefined,
  event: TimelineEvent,
): boolean {
  return anchor !== undefined
    && anchor.runId === event.runId
    && compareRunSequence(event.runSequence, anchor.runSequence) <= 0;
}

function compareSemanticItems(
  left: LiveConversationDisplayItem,
  right: LiveConversationDisplayItem,
): number {
  const run = left.runId.localeCompare(right.runId);
  if (run !== 0) return run;
  const sequence = compareRunSequence(left.runSequence, right.runSequence);
  return sequence !== 0 ? sequence : left.provisionalId.localeCompare(right.provisionalId);
}

function compareDeltaBuffers(left: LiveDeltaBuffer, right: LiveDeltaBuffer): number {
  const run = left.runId.localeCompare(right.runId);
  if (run !== 0) return run;
  const sequence = compareRunSequence(left.firstRunSequence, right.firstRunSequence);
  return sequence !== 0 ? sequence : left.channel.localeCompare(right.channel);
}

function compareTimelineEvents(left: TimelineEvent, right: TimelineEvent): number {
  const run = left.runId.localeCompare(right.runId);
  if (run !== 0) return run;
  const sequence = compareRunSequence(left.runSequence, right.runSequence);
  return sequence !== 0
    ? sequence
    : (left.itemId ?? left.approval?.callId ?? "")
        .localeCompare(right.itemId ?? right.approval?.callId ?? "");
}

function toolStatus(event: TimelineEvent): LiveConversationDisplayItem["status"] {
  // These two events describe provider-side tool-call assembly. The actual
  // execution lifecycle starts later with tool_progress, possibly after an
  // approval. Treating assembly completion as execution completion makes the
  // same provisional item regress from completed back to running.
  if (event.kind === "tool_started" || event.kind === "tool_completed") {
    return "requested";
  }
  switch (event.status) {
    case "approved":
      return "approved";
    case "denied":
      return "denied";
    case "failed":
    case "error":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "blocked":
      return "blocked";
    case "ok":
    case "success":
    case "succeeded":
      return "succeeded";
    case "complete":
    case "completed":
    case "ready":
      return "completed";
    default:
      return event.kind === "tool_result" ? "completed" : "running";
  }
}

function terminalStatus(event: TimelineEvent): ConversationTerminalStatus | undefined {
  const isTerminalEvent = event.kind === "run_finished"
    || event.kind === "run_failed"
    || event.kind === "run_blocked"
    || event.kind === "run_paused"
    || event.kind === "run_interrupted"
    || event.kind === "route_recovery_required"
    || event.kind === "run_cancelled";
  if (!isTerminalEvent) return undefined;
  // Durable or replayed adapters may preserve a more precise terminal status on a generic
  // `run_failed` envelope. Keep that typed status rather than manufacturing `failed`; this is
  // essential for recoverable paused/blocked and interrupted runs to retain their action model.
  switch (event.status) {
    case "succeeded":
    case "failed":
    case "cancelled":
    case "interrupted":
    case "paused":
    case "blocked":
    case "awaiting_user_input":
      return event.status;
    default:
      break;
  }
  if (event.kind === "run_finished") return "succeeded";
  if (event.kind === "run_failed") {
    return "failed";
  }
  if (event.kind === "run_blocked") return "blocked";
  if (event.kind === "run_paused") return "paused";
  if (event.kind === "run_interrupted") return "interrupted";
  if (event.kind === "route_recovery_required") return "failed";
  if (event.kind === "run_cancelled") return "cancelled";
  return undefined;
}

function isToolBoundary(kind: TimelineEvent["kind"]): boolean {
  return kind === "tool_started"
    || kind === "tool_completed"
    || kind === "tool_progress"
    || kind === "tool_result";
}

function isDecimalSequence(value: string): boolean {
  return /^(0|[1-9][0-9]*)$/.test(value);
}

function deltaBufferKey(runId: string, channel: LiveDeltaChannel): string {
  return `${runId}:${channel}`;
}

function approvalLifecycleKey(runId: string, callId: string): string {
  return JSON.stringify([runId, callId]);
}

function sameApprovalGuard(
  left: NonNullable<TimelineEvent["approval"]>,
  right: NonNullable<TimelineEvent["approval"]>,
): boolean {
  return left.callId === right.callId
    && left.toolName === right.toolName
    && left.approvalRequestId === right.approvalRequestId
    && left.toolCallHash === right.toolCallHash
    && left.policyVersion === right.policyVersion
    && left.expiresAtMs === right.expiresAtMs
    && left.sessionGrantAvailable === right.sessionGrantAvailable
    && left.toolInput === right.toolInput
    && left.operation === right.operation
    && left.risk === right.risk
    && left.snapshotRequired === right.snapshotRequired
    && left.previewTitle === right.previewTitle
    && left.previewSummary === right.previewSummary
    && left.previewBody === right.previewBody;
}

function sameAnchor(
  left: LiveProvisionalAnchor | undefined,
  right: LiveProvisionalAnchor | undefined,
): boolean {
  return left === right || (
    left !== undefined
    && right !== undefined
    && left.durableFrontier === right.durableFrontier
    && left.runId === right.runId
    && left.runSequence === right.runSequence
  );
}
