import { describe, expect, it } from "vitest";

import { translateEnglish } from "../../i18n";
import type { ConversationTimelineItem, LiveConversationDisplayItem } from "./continuityReducer";
import { projectConversationRows } from "./conversationRows";
import restoredCommand from "./fixtures/restored-command.json";

describe("canonical conversation rows", () => {
  it("restores a command and coalesces follow-up execution facts without a live overlay", () => {
    const make = (key: string, name: string): ConversationTimelineItem => ({ identity: key, source: "durable", item: {
      schemaVersion: 1, displayId: key, displayOrder: { sessionStreamSequence: key === "start" ? "2" : "3", subindex: 0 },
      sourceEventId: `event-${key}`, source: "durable_transcript", kind: "tool", runId: "run-restored", status: "running",
      content: { type: "tool", toolName: name, input: key === "start" ? restoredCommand.input : undefined,
        executionId: restoredCommand.executionId, executionStartedAtMs: restoredCommand.executionStartedAtMs,
        executionUpdatedAtMs: restoredCommand.executionUpdatedAtMs, output: restoredCommand.output,
        truncated: false, originalContentBytes: 0 },
    } });
    const rows = projectConversationRows([make("start", "exec_command"), make("wait", "exec_wait")], [], translateEnglish);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ kind: "tool", label: "exec_command", input: restoredCommand.input,
      status: restoredCommand.status, executionId: restoredCommand.executionId,
      executionStartedAtMs: restoredCommand.executionStartedAtMs, executionUpdatedAtMs: restoredCommand.executionUpdatedAtMs });
  });

  it("coalesces the same execution across runs and keeps its terminal state against late running receipts", () => {
    const make = (key: string, runId: string, status: "running" | "completed"): ConversationTimelineItem => ({ identity: key, source: "durable", item: {
      schemaVersion: 1, displayId: key, displayOrder: { sessionStreamSequence: "2", subindex: 0 },
      sourceEventId: `event-${key}`, source: "durable_transcript", kind: "tool", runId, status,
      content: { type: "tool", toolName: key === "start" ? "exec_command" : "exec_wait", input: key === "start" ? restoredCommand.input : undefined,
        executionId: restoredCommand.executionId, executionStartedAtMs: 1000, executionUpdatedAtMs: status === "completed" ? 4000 : 2000,
        truncated: false, originalContentBytes: 0 },
    } });
    const rows = projectConversationRows([make("start", "run-a", "running"), make("wait", "run-b", "completed"), make("stale", "run-a", "running")], [], translateEnglish);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ key: "start", kind: "tool", label: "exec_command", input: restoredCommand.input,
      status: "completed", executionId: restoredCommand.executionId, executionStartedAtMs: 1000, executionUpdatedAtMs: 4000 });
  });

  it("keeps a canonical terminal command when an old execution overlay is still retained", () => {
    const snapshot: LiveConversationDisplayItem = { provisionalId: "slot", runId: "run-a", runSequence: "1", kind: "tool", status: "running",
      executionId: restoredCommand.executionId, executionStartedAtMs: 1000, executionUpdatedAtMs: 2000,
      content: { type: "tool", toolName: "exec_command", output: "old output", truncated: false, originalContentBytes: 0 } };
    const durable: ConversationTimelineItem = { identity: "settled", source: "durable", item: {
      schemaVersion: 1, displayId: "settled", displayOrder: { sessionStreamSequence: "5", subindex: 0 },
      sourceEventId: "event-settled", source: "durable_transcript", kind: "tool", runId: "run-a", status: "interrupted", reconciles: ["slot"],
      content: { type: "tool", toolName: "exec_command", input: restoredCommand.input, executionId: restoredCommand.executionId,
        executionStartedAtMs: 1000, executionUpdatedAtMs: 4000, output: "final output", truncated: false, originalContentBytes: 12 },
    } };
    for (const [canonicalStatus, staleStatus] of [["interrupted", "running"], ["interrupted", "succeeded"], ["failed", "cancelled"]] as const) {
      const settled: ConversationTimelineItem = { ...durable, item: { ...durable.item, status: canonicalStatus } };
      expect(projectConversationRows([settled], [], translateEnglish, [{ ...snapshot, status: staleStatus }])).toMatchObject([
        { kind: "tool", status: canonicalStatus, text: "final output", executionUpdatedAtMs: 4000, input: restoredCommand.input },
      ]);
      const foreignLive: ConversationTimelineItem = { identity: "foreign-slot", source: "live", item: {
        ...snapshot, provisionalId: "foreign-slot", runId: "run-b", status: staleStatus,
        content: { type: "tool", toolName: "exec_wait", executionId: restoredCommand.executionId,
          output: "stale foreign output", executionUpdatedAtMs: 2000, truncated: false, originalContentBytes: 0 },
      } };
      expect(projectConversationRows([settled, foreignLive], [], translateEnglish)).toMatchObject([
        { kind: "tool", status: canonicalStatus, text: "final output", executionUpdatedAtMs: 4000 },
      ]);
    }
  });

  it("keeps live succeeded terminal state against a late running receipt from another run", () => {
    const make = (key: string, runId: string, status: "succeeded" | "running"): ConversationTimelineItem => ({ identity: key, source: "live", item: {
      provisionalId: key, runId, runSequence: "1", kind: "tool", status,
      content: { type: "tool", toolName: "exec_command", executionId: restoredCommand.executionId, input: restoredCommand.input,
        executionStartedAtMs: 1000, executionUpdatedAtMs: status === "succeeded" ? 4000 : 2000, truncated: false, originalContentBytes: 0 },
    } });
    expect(projectConversationRows([make("done", "run-a", "succeeded"), make("stale", "run-b", "running")], [], translateEnglish)).toMatchObject([
      { kind: "tool", status: "succeeded", executionUpdatedAtMs: 4000 },
    ]);
  });

  it("renders the canonical order without comparing duplicate text", () => {
    const items: ConversationTimelineItem[] = [
      durableMessage("user-1", "user", "same"),
      durableMessage("assistant-1", "assistant", "same"),
    ];

    expect(projectConversationRows(items, [], translateEnglish).map((row) => [row.key, row.kind, row.text])).toEqual([
      ["user-1", "user", "same"],
      ["assistant-1", "assistant", "same"],
    ]);
  });

  it("never renders a terminal marker as an assistant answer", () => {
    const terminal: ConversationTimelineItem = {
      identity: "terminal-1",
      source: "durable",
      item: {
        schemaVersion: 1,
        displayId: "terminal-1",
        displayOrder: { sessionStreamSequence: "2", subindex: 0 },
        sourceEventId: "event-terminal-1",
        source: "durable_run_event",
        kind: "terminal",
        runId: "run-1",
        status: "succeeded",
        content: { type: "terminal", safeSummary: "must not render", summaryTruncated: false },
      },
    };

    expect(projectConversationRows([terminal], [], translateEnglish)).toEqual([]);
  });

  it("preserves the user-selected skill on its message row", () => {
    const message = durableMessage("user-skill", "user", "research Chang'an");
    if (message.item.content.type !== "message") throw new Error("expected message content");
    message.item.content.skill = { id: "compat-skill-123", name: "唐代城市研究" };

    expect(projectConversationRows([message], [], translateEnglish)[0]).toMatchObject({
      kind: "user",
      skill: { id: "compat-skill-123", name: "唐代城市研究" },
    });
  });

  it("interleaves reasoning deltas with semantic live items by exact run sequence", () => {
    const assistant: ConversationTimelineItem = {
      identity: "assistant-live",
      source: "live",
      item: {
        provisionalId: "assistant-live",
        runId: "run-1",
        runSequence: "9007199254740993",
        kind: "assistant_message",
        status: "streaming",
        content: {
          type: "message",
          role: "assistant",
          text: "Finalizing",
          assistantPhase: "progress",
          imageAttachmentCount: 0,
          truncated: false,
          originalContentBytes: 10,
        },
      },
    };
    const rows = projectConversationRows([assistant], [{
      identity: "ephemeral:run-1:reasoning:9007199254740992",
      runId: "run-1",
      channel: "reasoning",
      firstRunSequence: "9007199254740992",
      lastRunSequence: "9007199254740992",
      fragments: new Map([["9007199254740992", "Inspecting"]]),
    }], translateEnglish);

    expect(rows.map((row) => [row.kind, row.text])).toEqual([
      ["reasoning", "Inspecting"],
      ["progress", "Finalizing"],
    ]);
  });

  it("preserves a bounded live tool input preview through row projection", () => {
    const tool: ConversationTimelineItem = {
      identity: "tool-live",
      source: "live",
      item: {
        provisionalId: "tool-live",
        runId: "run-1",
        runSequence: "4",
        kind: "tool",
        status: "running",
        content: {
          type: "tool",
          callId: "call-1",
          toolName: "bash",
          truncated: false,
          originalContentBytes: 0,
        },
        toolInput: "rg TODO",
      },
    };

    expect(projectConversationRows([tool], [], translateEnglish)[0]).toMatchObject({
      kind: "tool",
      input: "rg TODO",
    });
  });

  it("preserves opaque artifact capabilities without projecting a path", () => {
    const tool: ConversationTimelineItem = {
      identity: "tool-artifact",
      source: "durable",
      item: {
        schemaVersion: 1,
        displayId: "tool-artifact",
        displayOrder: { sessionStreamSequence: "5", subindex: 0 },
        sourceEventId: "event-tool-artifact",
        source: "durable_transcript",
        kind: "tool",
        runId: "run-1",
        status: "completed",
        content: {
          type: "tool",
          toolName: "shell",
          output: "bounded preview",
          truncated: true,
          originalContentBytes: 32,
          artifactRef: `ta1_${"a".repeat(32)}`,
          artifactAvailability: "available",
          observedBytes: 32,
          persistedBytes: 32,
          hasMore: true,
        },
      },
    };

    expect(projectConversationRows([tool], [], translateEnglish)[0]).toMatchObject({
      kind: "tool",
      artifactRef: `ta1_${"a".repeat(32)}`,
      artifactAvailability: "available",
      artifactHasMore: true,
      artifactPersistedBytes: 32,
    });
  });

  it("omits empty tool preambles without hiding visible preamble text", () => {
    const empty = durableAssistant("empty-preamble", "1", "", "tool_preamble");
    const visible = durableAssistant(
      "visible-preamble",
      "2",
      "I will inspect the affected file.",
      "tool_preamble",
    );

    expect(projectConversationRows([empty, visible], [], translateEnglish).map((row) => row.text)).toEqual([
      "I will inspect the affected file.",
    ]);
  });
});

function durableMessage(
  identity: string,
  role: "user" | "assistant",
  text: string,
): ConversationTimelineItem {
  return {
    identity,
    source: "durable",
    item: {
      schemaVersion: 1,
      displayId: identity,
      displayOrder: { sessionStreamSequence: role === "user" ? "1" : "2", subindex: 0 },
      sourceEventId: `event-${identity}`,
      source: "durable_transcript",
      kind: role === "user" ? "user_message" : "assistant_message",
      runId: "run-1",
      status: role === "user" ? "recorded" : "succeeded",
      content: {
        type: "message",
        role,
        text,
        assistantPhase: role === "assistant" ? "final_answer" : undefined,
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: text.length,
      },
    },
  };
}

function durableAssistant(
  identity: string,
  sequence: string,
  text: string,
  assistantPhase: "tool_preamble" | "progress" | "final_answer",
): ConversationTimelineItem {
  return {
    identity,
    source: "durable",
    item: {
      schemaVersion: 1,
      displayId: identity,
      displayOrder: { sessionStreamSequence: sequence, subindex: 0 },
      sourceEventId: `event-${identity}`,
      source: "durable_transcript",
      kind: "assistant_message",
      runId: "run-1",
      status: "recorded",
      content: {
        type: "message",
        role: "assistant",
        text,
        assistantPhase,
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: text.length,
      },
    },
  };
}
