import { describe, expect, it } from "vitest";

import { translateEnglish } from "../../i18n";
import type { ConversationTimelineItem } from "./continuityReducer";
import { projectConversationRows } from "./conversationRows";

describe("canonical conversation rows", () => {
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
