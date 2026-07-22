import type { TimelineEvent } from "../../types";
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

export type ApprovalLifecyclePhase = "pending" | "resolved" | "closed";

export interface ApprovalLifecycle {
  runId: string;
  callId: string;
  runSequence: string;
  phase: ApprovalLifecyclePhase;
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
  terminalSignals: ReadonlyMap<string, LiveTerminalSignal>;
}

export type LiveEventAction =
  | { type: "session_selected"; sessionId: string }
  | { type: "anchor_received"; sessionId: string; anchor?: LiveProvisionalAnchor }
  | { type: "event_received"; sessionId: string; event: TimelineEvent }
  | { type: "run_discarded"; sessionId: string; runId: string };

export function createLiveEventState(sessionId: string): LiveEventState {
  return {
    sessionId,
    semanticItems: new Map(),
    deltaBuffers: new Map(),
    controlEvents: new Map(),
    approvalLifecycles: new Map(),
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
    case "run_discarded":
      return discardRun(state, action.runId);
  }
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
  return [...state.controlEvents.values()].sort(compareTimelineEvents);
}

export function selectLatestPendingApproval(state: LiveEventState): TimelineEvent | undefined {
  const pending = selectPendingApprovalEvents(state);
  return pending[pending.length - 1];
}

export function selectTerminalSignals(state: LiveEventState): LiveTerminalSignal[] {
  return [...state.terminalSignals.values()].sort((left, right) => {
    const sequence = compareRunSequence(left.runSequence, right.runSequence);
    return sequence !== 0 ? sequence : left.runId.localeCompare(right.runId);
  });
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
  let next = controlUpdate.state;
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
  } else if (event.kind === "run_started") {
    next = clearRunDeltaBuffersThrough(next, event.runId, event.runSequence);
  }

  const item = semanticLiveItemFromTimelineEvent(event);
  return item === undefined || !controlUpdate.applySemantic
    ? next
    : receiveSemanticItem(next, item);
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

interface ApprovalControlUpdate {
  state: LiveEventState;
  applySemantic: boolean;
}

function updateControlEvents(
  state: LiveEventState,
  event: TimelineEvent,
): ApprovalControlUpdate {
  if (event.kind !== "approval_requested" && event.kind !== "approval_resolved") {
    return { state, applySemantic: true };
  }
  const callId = event.approval?.callId ?? event.itemId;
  if (callId === undefined) return { state, applySemantic: false };
  const key = approvalLifecycleKey(event.runId, callId);
  const current = state.approvalLifecycles.get(key);
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
      runSequence: event.runSequence,
      phase,
      resolutionFingerprint: event.kind === "approval_resolved"
        ? approvalResolutionFingerprint(event)
        : undefined,
    }, phase === "pending" ? event : undefined),
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
  if (event.kind === "tool_started") return "running";
  if (event.kind === "tool_completed") return "completed";
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
  if (event.kind === "run_finished") return "succeeded";
  if (event.kind === "run_failed") {
    return event.status === "interrupted" ? "interrupted" : "failed";
  }
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
