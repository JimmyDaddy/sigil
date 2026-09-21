import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToolCard } from "./ToolCard";
import {
  createLiveEventState,
  reduceLiveTimelineEvent,
  selectSemanticLiveItems,
} from "./features/conversation/liveEventReducer";
import type { TimelineEvent } from "./types";
import { createConversationContinuityState, reduceConversationContinuity, selectConversationTimeline } from "./features/conversation/continuityReducer";
import { projectConversationRows } from "./features/conversation/conversationRows";
import { translateEnglish } from "./i18n";
import { semanticLiveItemFromTimelineEvent } from "./features/conversation/liveEventReducer";

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("command visibility", () => {
  it("ticks elapsed time for a silent execution and freezes the actual final duration", () => {
    vi.useFakeTimers();
    vi.setSystemTime(10_000);
    const tool = { key: "quiet-command", toolName: "exec_command", input: "sleep 60", text: "", status: "pending" };
    const view = render(<ToolCard tool={tool} />);
    expect(screen.queryByLabelText("Execution elapsed")).toBeNull();
    view.rerender(<ToolCard tool={{ ...tool, status: "running", executionStartedAtMs: 9_000 }} />);
    expect(screen.getByLabelText("Execution elapsed").textContent).toBe("1.0 s");
    act(() => { vi.advanceTimersByTime(2_000); });
    expect(screen.getByLabelText("Execution elapsed").textContent).toBe("3.0 s");
    view.rerender(<ToolCard tool={{ ...tool, status: "cancelled", executionStartedAtMs: 9_000, executionUpdatedAtMs: 11_500 }} />);
    act(() => { vi.advanceTimersByTime(5_000); });
    expect(screen.getByLabelText("Execution elapsed").textContent).toBe("2.5 s");
  });

  it("renders execution previews across provider attempts and merges wait and terminal lifecycle into the command", () => {
    let live = createLiveEventState("session-command");
    let continuity = createConversationContinuityState("session-command");
    const base: TimelineEvent = { workspaceId: "workspace-command", sessionId: "session-command", runId: "run-command",
      sequence: 1, runSequence: "1", replayable: true, kind: "tool_completed", provisionalId: "command-card",
      itemId: "call-command", toolName: "exec_command", toolInput: "cargo check --workspace" };
    const ingest = (event: TimelineEvent) => {
      live = reduceLiveTimelineEvent(live, event);
      const item = semanticLiveItemFromTimelineEvent(event);
      if (item !== undefined) continuity = reduceConversationContinuity(continuity, { type: "live_item_received", sessionId: base.sessionId, item });
    };
    ingest(base);
    const executionPreview: TimelineEvent = { ...base, replayable: false, provisionalId: undefined, toolInput: undefined,
      kind: "tool_progress", executionId: "process-1", status: "running", executionStartedAtMs: 10, executionUpdatedAtMs: 20,
      livePreview: { slotId: "process-1", revision: "3", baseSequence: "1", truncated: false } };
    ingest(executionPreview);
    ingest({ ...base, sequence: 2, runSequence: "2", kind: "assistant_message", provisionalId: undefined, itemId: undefined, toolName: undefined, toolInput: undefined, text: "waiting" });
    ingest({ ...executionPreview, livePreview: { ...executionPreview.livePreview!, revision: "4" }, text: "Checking workspace" });
    const rows = () => projectConversationRows(selectConversationTimeline(continuity), [], translateEnglish, selectSemanticLiveItems(live));
    const running = rows().filter((row) => row.kind === "tool");
    expect(running).toHaveLength(1);
    expect(running[0]).toMatchObject({ input: "cargo check --workspace", status: "running", text: "Checking workspace" });
    ingest({ ...base, sequence: 3, runSequence: "3", kind: "tool_result", executionId: "process-1", status: "running", toolInput: undefined, text: "" });
    expect(rows().find((row) => row.kind === "tool")?.status).toBe("running");
    ingest({ ...base, sequence: 4, runSequence: "4", kind: "tool_result", provisionalId: "wait-card", itemId: "call-wait", toolName: "exec_wait",
      executionId: "process-1", status: "running", toolInput: undefined, text: "Still checking" });
    expect(rows().filter((row) => row.kind === "tool")).toHaveLength(1);
    expect(rows().find((row) => row.kind === "tool")?.status).toBe("running");
    ingest({ ...base, sequence: 5, runSequence: "5", kind: "terminal_lifecycle", provisionalId: undefined, itemId: "process-1", toolName: undefined, toolInput: undefined,
      terminalTask: { taskId: "process-1", generation: 2, status: "cancelled", readiness: "none", totalOutputBytes: 14, emittedAtMs: 100 } });
    const final = rows().filter((row) => row.kind === "tool");
    expect(final).toHaveLength(1);
    expect(final[0]).toMatchObject({ input: "cargo check --workspace", status: "cancelled", executionStartedAtMs: 10, executionUpdatedAtMs: 100 });
    expect(continuity.contractError).toBeUndefined();
  });

  it("keeps silent command input visible through argument completion, progress and failure", () => {
    let state = createLiveEventState("session-command");
    const callEvent = (kind: TimelineEvent["kind"], sequence: number): TimelineEvent => ({
      workspaceId: "workspace-command", sessionId: "session-command", runId: "run-command", sequence,
      runSequence: String(sequence), provisionalId: "command-card", replayable: true,
      kind, itemId: "call-command", toolName: "exec_command",
    });
    const states = [
      { ...callEvent("tool_completed", 1), toolInput: "cargo check --workspace", status: "completed" },
      { ...callEvent("tool_progress", 2), status: "running" },
      { ...callEvent("tool_result", 3), status: "failed", text: "compile failed" },
    ];
    const labels = ["Requested", "Running", "Failed"];
    for (const [index, event] of states.entries()) {
      state = reduceLiveTimelineEvent(state, event);
      const items = selectSemanticLiveItems(state);
      expect(items).toHaveLength(1);
      const item = items[0]!;
      expect(item.content.type).toBe("tool");
      if (item.content.type !== "tool") throw new Error("expected command item");
      const view = render(<ToolCard tool={{ key: item.provisionalId, toolName: "exec_command",
        input: item.toolInput, text: item.content.output ?? "", status: item.status }} />);
      expect(screen.getByLabelText("exec_command input").textContent?.trimEnd()).toBe("cargo check --workspace");
      expect(screen.getByText(labels[index]!)).toBeTruthy();
      view.unmount();
    }
  });

  it("shows a bounded multiline command and expands the complete safe input while running", async () => {
    const user = userEvent.setup();
    const command = "cargo check --workspace\nprintf [redacted]\nprintf third-line\nprintf last-line";
    render(<ToolCard tool={{ key: "long-command", toolName: "exec_command", input: command, text: "", status: "running" }} />);
    const input = screen.getByLabelText("exec_command input");
    expect(input.textContent).toContain("cargo check --workspace");
    expect(input.textContent).toContain("[redacted]");
    expect(input.textContent).not.toContain("last-line");
    await user.click(screen.getByRole("button", { name: "Show full command" }));
    expect(input.textContent?.trimEnd()).toBe(command);
    expect(screen.getByRole("button", { name: "Collapse command" }).getAttribute("aria-expanded")).toBe("true");
  });
});
