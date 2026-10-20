import type { Translate } from "../../i18n";
import type { MessageView } from "../../Message";
import type { ToolArtifactAvailability } from "../../types";
import type { ConversationTimelineItem, LiveConversationDisplayItem } from "./continuityReducer";
import {
  compareRunSequence,
  selectDeltaText,
  type ApprovalPresentation,
  type LiveDeltaBuffer,
} from "./liveEventReducer";

interface TimelineRowBase {
  key: string;
  label: string;
  text: string;
  skill?: MessageView["skill"];
  status?: string;
  contentTruncated?: boolean;
  images?: MessageView["images"];
  presentation?: MessageView["presentation"];
  sourceStreamSequence?: string;
}

export type ConversationTimelineRow =
  | (TimelineRowBase & { kind: MessageView["kind"] })
  | (TimelineRowBase & {
      kind: "tool";
      input?: string;
      executionId?: string;
      executionRunId?: string;
      executionStartedAtMs?: number;
      executionUpdatedAtMs?: number;
      artifactRef?: string;
      artifactAvailability?: ToolArtifactAvailability;
      artifactHasMore?: boolean;
      artifactPersistedBytes?: number;
    });

export function projectConversationRows(
  items: readonly ConversationTimelineItem[],
  deltaBuffers: readonly LiveDeltaBuffer[],
  t: Translate,
  toolSnapshots: readonly LiveConversationDisplayItem[] = [],
  visibleApproval?: ApprovalPresentation,
): ConversationTimelineRow[] {
  const pendingApprovalAnchored = items.some((entry) => entry.item.content.type === "approval"
    && isVisibleApprovalPlaceholder(entry, visibleApproval, undefined));
  const projectEntry = (entry: ConversationTimelineItem): ConversationTimelineRow[] => {
    const identities = entry.source === "durable" ? entry.item.reconciles ?? [] : [entry.identity];
    const snapshot = toolSnapshots.find((candidate) => candidate.runId === entry.item.runId
      && candidate.content.type === "tool" && identities.includes(candidate.provisionalId));
    if ((entry.item.content.type !== "tool" || pendingApprovalAnchored)
      && isVisibleApprovalPlaceholder(entry, visibleApproval, snapshot)) return [];
    const rows = projectDisplayItem(entry.identity, entry.item, t).map((row) => ({
      ...row,
      sourceStreamSequence: entry.source === "durable" ? entry.item.displayOrder.sessionStreamSequence : undefined,
    }));
    if (snapshot?.content.type !== "tool") return rows;
    const output = snapshot.content.output;
    return rows.map((row) => {
      if (row.kind !== "tool") return row;
      const sequenceIsCurrent = entry.item.runSequence === undefined
        || compareRunSequence(snapshot.runSequence, entry.item.runSequence) >= 0;
      // Canonical terminal rows already incorporate durable owner generations. A retained live
      // snapshot without a newer owner generation cannot refine their final state or output.
      const keepsTerminalState = !isTerminalCommandStatus(row.status)
        || (entry.source !== "durable" && isTerminalCommandStatus(snapshot.status));
      if (!sequenceIsCurrent || !keepsTerminalState) return { ...row, input: row.input ?? snapshot.toolInput };
      return { ...row, input: snapshot.toolInput ?? row.input, status: snapshot.status,
        ...(snapshot.executionId === undefined ? {} : {
          executionId: snapshot.executionId, executionRunId: snapshot.runId,
          executionStartedAtMs: snapshot.executionStartedAtMs ?? row.executionStartedAtMs,
          executionUpdatedAtMs: snapshot.executionUpdatedAtMs ?? row.executionUpdatedAtMs,
          text: output ?? row.text,
        }),
      };
    });
  };
  const durableTerminals = new Set(items.flatMap((entry) => entry.source === "durable"
    && entry.item.content.type === "tool" && entry.item.content.executionId !== undefined
    && isTerminalCommandStatus(entry.item.status) ? [entry.item.content.executionId] : []));
  const durableRows = items
    .filter((entry) => entry.source === "durable")
    .flatMap(projectEntry);
  const liveEntries: LiveRowEntry[] = items
    .filter((entry) => entry.source === "live")
    .map((entry) => ({
      runId: entry.item.runId,
      runSequence: entry.item.runSequence,
      identity: entry.identity,
      rows: projectEntry(entry),
    }));
  const deltaEntries: LiveRowEntry[] = deltaBuffers.map((buffer) => ({
    runId: buffer.runId,
    runSequence: buffer.firstRunSequence,
    identity: buffer.identity,
    rows: [{
      key: buffer.identity,
      kind: buffer.channel === "reasoning" ? "reasoning" : "progress",
      label: buffer.channel === "reasoning" ? t("working") : "Sigil",
      text: `${selectDeltaText(buffer)}${buffer.truncated === true ? "\n…" : ""}`,
      status: "streaming",
    }],
  }));
  return coalesceExecutionRows([
    ...durableRows,
    ...[...liveEntries, ...deltaEntries]
      .sort(compareLiveRowEntries)
      .flatMap((entry) => entry.rows),
  ], durableRows.length, durableTerminals);
}

function isVisibleApprovalPlaceholder(
  entry: ConversationTimelineItem,
  presentation: ApprovalPresentation | undefined,
  snapshot: LiveConversationDisplayItem | undefined,
): boolean {
  const event = presentation?.event;
  const approval = event?.approval;
  if (presentation?.phase !== "pending" || approval === undefined || event === undefined
    || event.provisionalId === undefined || entry.item.runId !== event.runId) return false;
  const item = entry.item;
  const content = item.content;
  const sameCall = content.type === "tool" || content.type === "approval"
    ? content.callId === approval.callId || event.displayCallId !== undefined && content.callId === event.displayCallId
    : false;
  if (!sameCall) return false;
  if (content.type === "approval") {
    // Canonical approval content has no request ID; require its exact reconciliation slot
    // in addition to the currently guarded call rather than matching tool names or copy.
    const identities = entry.source === "durable" ? entry.item.reconciles ?? [] : [entry.identity];
    return item.status === "waiting_for_approval" && content.decision === undefined
      && event.provisionalId !== undefined && identities.includes(event.provisionalId);
  }
  if (content.type !== "tool" || !["requested", "pending", "waiting_for_approval"].includes(item.status)) return false;
  if (entry.source === "durable" && (entry.item.reconciles?.length ?? 0) === 0) return false;
  if (entry.source === "live" && entry.item.toolApprovalRequestId !== undefined
    && entry.item.toolApprovalRequestId !== approval.approvalRequestId) return false;
  return !hasToolResult(content) && !("executionId" in item && item.executionId !== undefined)
    && !("executionStartedAtMs" in item && item.executionStartedAtMs !== undefined)
    && !("executionUpdatedAtMs" in item && item.executionUpdatedAtMs !== undefined)
    && (snapshot === undefined || snapshot.content.type === "tool" && !hasToolResult(snapshot.content)
      && snapshot.executionId === undefined && snapshot.executionStartedAtMs === undefined && snapshot.executionUpdatedAtMs === undefined
      && ["requested", "pending", "waiting_for_approval"].includes(snapshot.status)
      && (snapshot.toolApprovalRequestId === undefined || snapshot.toolApprovalRequestId === approval.approvalRequestId));
}

function hasToolResult(content: Extract<LiveConversationDisplayItem["content"], { type: "tool" }>): boolean {
  return content.output !== undefined || content.executionId !== undefined || content.artifactRef !== undefined
    || content.executionStartedAtMs !== undefined || content.executionUpdatedAtMs !== undefined || content.hasMore === true
    || content.artifactAvailability !== undefined || content.truncated === true || (content.originalContentBytes ?? 0) > 0
    || (content.observedBytes ?? 0) > 0 || (content.persistedBytes ?? 0) > 0;
}

function isTerminalCommandStatus(status: string | undefined): boolean {
  return ["completed", "succeeded", "success", "failed", "error", "cancelled", "interrupted", "timed_out", "cleanup_incomplete"].includes(status ?? "");
}

function coalesceExecutionRows(rows: ConversationTimelineRow[], durableRowCount: number, durableTerminals: ReadonlySet<string>): ConversationTimelineRow[] {
  const result: ConversationTimelineRow[] = [];
  const executions = new Map<string, number>();
  for (const [rowIndex, row] of rows.entries()) {
    if (row.kind !== "tool" || row.executionId === undefined) { result.push(row); continue; }
    // Rows belong to one session; the authority-issued execution ID spans follow-up runs.
    const key = row.executionId;
    const previousIndex = executions.get(key);
    const previous = previousIndex === undefined ? undefined : result[previousIndex];
    if (rowIndex >= durableRowCount && durableTerminals.has(key)) continue;
    if (previousIndex !== undefined && previous?.kind === "tool") {
      const previousTerminal = isTerminalCommandStatus(previous.status);
      if (previousTerminal && ["requested", "pending", "running", "streaming"].includes(row.status ?? "")) continue;
      result[previousIndex] = { ...row, text: row.text || previous.text,
        artifactRef: row.artifactRef ?? previous.artifactRef,
        artifactAvailability: row.artifactAvailability ?? previous.artifactAvailability,
        artifactHasMore: row.artifactRef === undefined ? previous.artifactHasMore : row.artifactHasMore,
        artifactPersistedBytes: row.artifactPersistedBytes ?? previous.artifactPersistedBytes, key: previous.key, label: previous.label, input: previous.input ?? row.input,
        executionStartedAtMs: previous.executionStartedAtMs ?? row.executionStartedAtMs };
    } else { executions.set(key, result.length); result.push(row); }
  }
  return result;
}

interface LiveRowEntry {
  readonly runId: string;
  readonly runSequence: string;
  readonly identity: string;
  readonly rows: ConversationTimelineRow[];
}

function compareLiveRowEntries(left: LiveRowEntry, right: LiveRowEntry): number {
  const run = left.runId.localeCompare(right.runId);
  if (run !== 0) return run;
  const sequence = compareRunSequence(left.runSequence, right.runSequence);
  return sequence !== 0 ? sequence : left.identity.localeCompare(right.identity);
}

function projectDisplayItem(
  identity: string,
  item: ConversationTimelineItem["item"],
  t: Translate,
): ConversationTimelineRow[] {
  const content = item.content;
  switch (content.type) {
    case "message": {
      const attachmentText = content.imageAttachmentCount > 0
        ? `${content.imageAttachmentCount} image attachment${content.imageAttachmentCount === 1 ? "" : "s"} recorded.`
        : "";
      const text = (content.text ?? attachmentText) || "";
      if (
        content.role === "assistant"
        && content.assistantPhase === "tool_preamble"
        && text.trim() === ""
      ) return [];
      const previewStatus = content.truncated
        ? `preview · ${content.originalContentBytes} bytes`
        : content.imageAttachmentCount > 0
          ? `${content.imageAttachmentCount} attachment${content.imageAttachmentCount === 1 ? "" : "s"}`
          : undefined;
      if (content.role === "user") {
        return [{
          key: identity,
          kind: "user",
          label: t("you"),
          text,
          skill: content.skill,
          images: content.imageAttachments,
          status: previewStatus,
          contentTruncated: content.truncated,
        }];
      }
      const kind = content.assistantPhase === "tool_preamble" || content.assistantPhase === "progress"
        ? "progress"
        : "assistant";
      return [{
        key: identity,
        kind,
        label: kind === "progress" ? t("progress") : "Sigil",
        text,
        status: previewStatus ?? (item.status === "streaming" ? "streaming" : undefined),
        contentTruncated: content.truncated,
      }];
    }
    case "reasoning":
      return [{
        key: identity,
        kind: "reasoning",
        label: item.status === "streaming" ? t("working") : t("reasoning"),
        text: content.text,
        contentTruncated: content.truncated,
        status: content.truncated
          ? `preview · ${content.originalContentBytes} bytes`
          : item.status === "streaming" ? "streaming" : undefined,
      }];
    case "tool":
      return [{
        key: identity,
        kind: "tool",
        label: content.toolName ?? t("toolResult"),
        text: content.output ?? "",
        input: content.input ?? ("toolInput" in item ? item.toolInput : undefined),
        executionId: content.executionId,
        executionRunId: item.runId,
        executionStartedAtMs: content.executionStartedAtMs,
        executionUpdatedAtMs: content.executionUpdatedAtMs,
        status: item.status,
        artifactRef: content.artifactRef,
        artifactAvailability: content.artifactAvailability,
        artifactHasMore: content.hasMore,
        artifactPersistedBytes: content.persistedBytes,
      }];
    case "approval": {
      const approved = item.status === "approved";
      const denied = item.status === "denied";
      const cancelled = item.status === "cancelled" || item.status === "interrupted";
      const closed = cancelled || ["completed", "failed", "expired"].includes(item.status);
      return [{
        key: identity,
        kind: "notice",
        label: approved ? t("approvalApproved") : denied ? t("approvalDenied") : closed ? t("approvalClosed") : t("approvalRequired"),
        text: approved
          ? t("approvalApprovedDetail")
          : denied ? t("approvalDeniedDetail") : cancelled ? t("approvalCancelledDetail")
            : closed ? t("approvalClosedDetail") : t("toolWaitingDecision", { tool: content.toolName }),
        status: approved ? "approved" : denied ? "denied" : closed ? item.status : "waiting",
        ...(approved ? { presentation: "approval_receipt" as const } : {}),
      }];
    }
    case "checkpoint":
      return [{
        key: identity,
        kind: content.outcome === "conflict" ? "error" : "notice",
        label: "Checkpoint",
        text: content.conflictReason ?? content.checkpointId ?? content.outcome,
        status: content.outcome,
      }];
    case "notice":
      return [{ key: identity, kind: "notice", label: t("notice"), text: content.text }];
    case "terminal":
      return [];
  }
}
