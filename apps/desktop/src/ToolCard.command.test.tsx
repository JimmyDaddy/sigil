import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToolCard } from "./ToolCard";
import {
  createLiveEventState,
  reduceLiveTimelineEvent,
  selectSemanticLiveItems,
} from "./features/conversation/liveEventReducer";
import type { TimelineEvent, ToolArtifactPage, ToolArtifactSelector } from "./types";
import { createConversationContinuityState, reduceConversationContinuity, selectConversationTimeline } from "./features/conversation/continuityReducer";
import { projectConversationRows } from "./features/conversation/conversationRows";
import { translateEnglish } from "./i18n";
import { semanticLiveItemFromTimelineEvent } from "./features/conversation/liveEventReducer";

afterEach(() => { cleanup(); vi.useRealTimers(); });

function deferredArtifactRead(artifactRef: string) {
  let resolve!: (page: ToolArtifactPage) => void;
  let reject!: (error: Error) => void;
  const pending = new Promise<ToolArtifactPage>((success, failure) => {
    resolve = success;
    reject = failure;
  });
  const read = vi.fn((_selector: ToolArtifactSelector) => pending);
  return {
    read,
    pending,
    complete: (body: string) => resolve({
      schemaVersion: 1, requestScope: "deferred-artifact", artifactRef,
      selector: read.mock.calls[0]![0], body, bodyEncoding: "utf8",
      returnedBytes: body.length, pageSha256: `sha256:${"b".repeat(64)}`,
      artifactSha256: `sha256:${"c".repeat(64)}`, eof: true, matchCount: 0,
    }),
    fail: () => reject(new Error("late artifact read failure")),
  };
}

it.each(["tool key", "artifact ref"])("ignores a late manual output after changing the %s and loading the new short output", async (scope) => {
  const user = userEvent.setup();
  const artifactRef = `ta1_${"a".repeat(32)}`;
  const nextArtifactRef = scope === "artifact ref" ? `ta1_${"d".repeat(32)}` : artifactRef;
  const previous = deferredArtifactRead(artifactRef);
  const next = deferredArtifactRead(nextArtifactRef);
  const tool = { key: "previous-call", toolName: "exec_command", text: "bounded preview",
    status: "succeeded", artifactRef, artifactAvailability: "available" as const, artifactPersistedBytes: 32_000 };
  const view = render(<ToolCard tool={tool} onReadArtifact={previous.read} />);
  await user.click(screen.getByText("Full output and search"));
  await user.click(screen.getByRole("button", { name: "View saved output" }));
  expect(previous.read).toHaveBeenCalledExactlyOnceWith({ kind: "byte_slice", offset: 0, limit: 16_384 });

  view.rerender(<ToolCard tool={{ ...tool,
    key: scope === "tool key" ? "next-call" : tool.key,
    artifactRef: nextArtifactRef, artifactPersistedBytes: 32,
  }} onReadArtifact={next.read} />);
  await waitFor(() => expect(next.read).toHaveBeenCalledExactlyOnceWith({ kind: "byte_slice", offset: 0, limit: 4_096 }));
  await act(async () => { next.complete("Current output B"); await next.pending; });
  expect((await screen.findByLabelText("exec_command saved output page")).textContent).toContain("Current output B");
  await act(async () => { previous.complete("Obsolete output A"); await previous.pending; });
  expect(screen.getByLabelText("exec_command saved output page").textContent).toContain("Current output B");
  expect(screen.queryByText("Obsolete output A")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("keeps the current read busy when an obsolete manual request fails", async () => {
  const user = userEvent.setup();
  const artifactRef = `ta1_${"a".repeat(32)}`;
  const nextArtifactRef = `ta1_${"d".repeat(32)}`;
  const previous = deferredArtifactRead(artifactRef);
  const next = deferredArtifactRead(nextArtifactRef);
  const tool = { key: "previous-call", toolName: "exec_command", text: "bounded preview",
    status: "succeeded", artifactRef, artifactAvailability: "available" as const, artifactPersistedBytes: 32_000 };
  const view = render(<ToolCard tool={tool} onReadArtifact={previous.read} />);
  await user.click(screen.getByText("Full output and search"));
  await user.click(screen.getByRole("button", { name: "View saved output" }));
  view.rerender(<ToolCard tool={{ ...tool, key: "next-call", artifactRef: nextArtifactRef, artifactPersistedBytes: 32 }} onReadArtifact={next.read} />);
  await waitFor(() => expect(next.read).toHaveBeenCalledOnce());
  await act(async () => { previous.fail(); await previous.pending.catch(() => {}); });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByText("Loading output…")).toBeTruthy();
  expect((screen.getByRole("button", { name: "View saved output" }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => { next.complete("Current output B"); await next.pending; });
  expect((await screen.findByLabelText("exec_command saved output page")).textContent).toContain("Current output B");
});

it("ignores an obsolete automatic read after a newer manual output succeeds", async () => {
  const user = userEvent.setup();
  const artifactRef = `ta1_${"a".repeat(32)}`;
  const nextArtifactRef = `ta1_${"d".repeat(32)}`;
  const previous = deferredArtifactRead(artifactRef);
  const next = deferredArtifactRead(nextArtifactRef);
  const tool = { key: "previous-call", toolName: "exec_command", text: "bounded preview",
    status: "succeeded", artifactRef, artifactAvailability: "available" as const, artifactPersistedBytes: 32 };
  const view = render(<ToolCard tool={tool} onReadArtifact={previous.read} />);
  await waitFor(() => expect(previous.read).toHaveBeenCalledOnce());
  view.rerender(<ToolCard tool={{ ...tool, key: "next-call", artifactRef: nextArtifactRef, artifactPersistedBytes: 32_000 }} onReadArtifact={next.read} />);
  await user.click(screen.getByText("Full output and search"));
  await user.click(screen.getByRole("button", { name: "View saved output" }));
  await act(async () => { next.complete("Current output B"); await next.pending; });
  expect((await screen.findByLabelText("exec_command saved output page")).textContent).toContain("Current output B");
  await act(async () => { previous.complete("Obsolete output A"); await previous.pending; });
  expect(screen.getByLabelText("exec_command saved output page").textContent).toContain("Current output B");
  expect(screen.queryByText("Obsolete output A")).toBeNull();
});

it("shows a short saved output directly and leaves search in details", async () => {
  const artifactRef = `ta1_${"a".repeat(32)}`;
  const body = "Ran 55 tests\nOK\n";
  const read = vi.fn(async (selector: ToolArtifactSelector) => ({
    schemaVersion: 1 as const, requestScope: "short-output", artifactRef, selector,
    body, bodyEncoding: "utf8" as const, returnedBytes: body.length,
    pageSha256: `sha256:${"b".repeat(64)}`, artifactSha256: `sha256:${"c".repeat(64)}`,
    eof: true, matchCount: 0,
  }));
  render(<ToolCard tool={{ key: "short-saved", toolName: "exec_command", text: "saved output",
    status: "succeeded", artifactRef, artifactAvailability: "available", artifactPersistedBytes: body.length,
  }} onReadArtifact={read} />);
  expect((await screen.findByLabelText("exec_command saved output page")).textContent?.trimEnd()).toBe(body.trimEnd());
  expect(read).toHaveBeenCalledExactlyOnceWith({ kind: "byte_slice", offset: 0, limit: 4_096 });
  expect(screen.getByText("Full output and search").closest("details")?.open).toBe(false);
  expect(screen.queryByText(/saved bytes are available|bounded pages/)).toBeNull();
});

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
