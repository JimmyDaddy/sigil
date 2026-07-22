import type { Translate } from "../../i18n";
import type { MessageView } from "../../Message";
import type { ConversationTimelineItem } from "./continuityReducer";
import {
  compareRunSequence,
  selectDeltaText,
  type LiveDeltaBuffer,
} from "./liveEventReducer";

interface TimelineRowBase {
  key: string;
  label: string;
  text: string;
  status?: string;
}

export type ConversationTimelineRow =
  | (TimelineRowBase & { kind: MessageView["kind"] })
  | (TimelineRowBase & { kind: "tool"; input?: string });

export function projectConversationRows(
  items: readonly ConversationTimelineItem[],
  deltaBuffers: readonly LiveDeltaBuffer[],
  t: Translate,
): ConversationTimelineRow[] {
  const durableRows = items
    .filter((entry) => entry.source === "durable")
    .flatMap(({ identity, item }) => projectDisplayItem(identity, item, t));
  const liveEntries: LiveRowEntry[] = items
    .filter((entry) => entry.source === "live")
    .map(({ identity, item }) => ({
      runId: item.runId,
      runSequence: item.runSequence,
      identity,
      rows: projectDisplayItem(identity, item, t),
    }));
  const deltaEntries: LiveRowEntry[] = deltaBuffers.map((buffer) => ({
    runId: buffer.runId,
    runSequence: buffer.firstRunSequence,
    identity: buffer.identity,
    rows: [{
      key: buffer.identity,
      kind: buffer.channel === "reasoning" ? "reasoning" : "progress",
      label: buffer.channel === "reasoning" ? t("working") : "Sigil",
      text: selectDeltaText(buffer),
      status: "streaming",
    }],
  }));
  return [
    ...durableRows,
    ...[...liveEntries, ...deltaEntries]
      .sort(compareLiveRowEntries)
      .flatMap((entry) => entry.rows),
  ];
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
        return [{ key: identity, kind: "user", label: t("you"), text, status: previewStatus }];
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
      }];
    }
    case "reasoning":
      return [{
        key: identity,
        kind: "reasoning",
        label: item.status === "streaming" ? t("working") : t("reasoning"),
        text: content.text,
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
        input: "toolInput" in item ? item.toolInput : undefined,
        status: item.status,
      }];
    case "approval": {
      const approved = item.status === "approved";
      const denied = item.status === "denied";
      return [{
        key: identity,
        kind: "notice",
        label: approved ? t("approvalApproved") : denied ? t("approvalDenied") : t("approvalRequired"),
        text: approved
          ? t("approvalApprovedDetail")
          : denied ? t("approvalDeniedDetail") : t("toolWaitingDecision", { tool: content.toolName }),
        status: approved ? "approved" : denied ? "denied" : "waiting",
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
