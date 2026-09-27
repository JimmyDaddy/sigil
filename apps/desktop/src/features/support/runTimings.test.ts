import { createHash, webcrypto } from "node:crypto";
import { afterEach, expect, test, vi } from "vitest";

import type { TimelineEvent } from "../../types";
import { createLiveEventState, reduceLiveTimelineEvent } from "../conversation/liveEventReducer";

import { beginSubmissionTiming, bindSubmissionRun, observeFeedbackFrame, observeRunTiming, observeCommittedRunTimings, rendererRunTimings } from "./runTimings";

afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

test("export is workspace scoped, bounded, correlated and excludes raw run identities", async () => {
  vi.stubGlobal("crypto", webcrypto);
  for (let index = 0; index < 40; index += 1) {
    bindSubmissionRun(beginSubmissionTiming("workspace-a"), `run-${index}`);
  }
  bindSubmissionRun(beginSubmissionTiming("workspace-b"), "other-run");
  const timings = await rendererRunTimings("workspace-a");
  expect(timings).toHaveLength(62);
  expect(timings[timings.length - 1]?.runKey).toBe(createHash("sha256").update("run-39").digest("hex"));
  expect(JSON.stringify(timings)).not.toContain("run-");
});

test("early stream content is correlated after admission and cancellation requires a request", async () => {
  vi.stubGlobal("crypto", webcrypto);
  const clock = vi.spyOn(performance, "now").mockReturnValue(100);
  const submission = beginSubmissionTiming("early-workspace");
  const before = await rendererRunTimings("early-workspace");
  clock.mockReturnValue(105);
  observeRunTiming("early-workspace", "early-run", "first_content");
  clock.mockReturnValue(110);
  bindSubmissionRun(submission, "early-run");
  observeRunTiming("early-workspace", "early-run", "cancellation_settled");
  const phases = await rendererRunTimings("early-workspace");
  expect(phases[0].submissionKey).toBe(before[0].submissionKey);
  expect(phases[0].runKey).not.toBe(before[0].runKey);
  expect(phases.find((entry) => entry.phase === "first_content")?.elapsedUs).toBe(5_000);
  expect(phases.find((entry) => entry.phase === "admission")?.elapsedUs).toBe(10_000);
  expect(phases.some((entry) => entry.phase === "cancellation_settled")).toBe(false);
});

test("feedback timing waits for a rendered frame and an unavailable hasher does not block export", async () => {
  vi.stubGlobal("crypto", webcrypto);
  const callbacks: FrameRequestCallback[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => callbacks.push(callback));
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  const submission = beginSubmissionTiming("frame-workspace");
  const cleanup = observeFeedbackFrame(submission);
  expect((await rendererRunTimings("frame-workspace")).some((entry) => entry.phase === "first_feedback_frame")).toBe(false);
  callbacks[0](0);
  callbacks[1](0);
  expect((await rendererRunTimings("frame-workspace")).some((entry) => entry.phase === "first_feedback_frame")).toBe(true);
  cleanup();
  vi.stubGlobal("crypto", {});
  expect(await rendererRunTimings("frame-workspace")).toEqual([]);
});

test("committed content excludes retired attempts and accepts the next attempt", async () => {
  vi.stubGlobal("crypto", webcrypto);
  const workspaceId = "retired-attempt-workspace";
  const runId = "retired-attempt-run";
  const clock = vi.spyOn(performance, "now").mockReturnValue(100);
  bindSubmissionRun(beginSubmissionTiming(workspaceId), runId);
  let state = createLiveEventState("session-1");
  const receive = (overrides: Partial<TimelineEvent>) => {
    state = reduceLiveTimelineEvent(state, timingEvent(workspaceId, runId, overrides));
    observeCommittedRunTimings(workspaceId, state);
  };
  receive({ kind: "run_started", runSequence: "3", replayable: true });
  receive({ kind: "assistant_delta", runSequence: "3", text: "", livePreview: preview("attempt-1", "1", "3") });
  receive({ kind: "provider_turn_partial_output_discarded", runSequence: "4", replayable: true });
  receive({ kind: "assistant_delta", runSequence: "4", text: "late discarded output", livePreview: preview("attempt-1", "2", "4") });
  expect((await rendererRunTimings(workspaceId)).some((entry) => entry.phase === "first_content")).toBe(false);
  clock.mockReturnValue(115);
  receive({ kind: "assistant_delta", runSequence: "4", text: "current output", livePreview: preview("attempt-2", "3", "4") });
  expect((await rendererRunTimings(workspaceId)).find((entry) => entry.phase === "first_content")?.elapsedUs).toBe(15_000);
});

test("terminal-before-admission clears early content and fences later previews", async () => {
  vi.stubGlobal("crypto", webcrypto);
  const workspaceId = "terminal-before-admission-workspace";
  const runId = "terminal-before-admission-run";
  const submission = beginSubmissionTiming(workspaceId);
  let state = createLiveEventState("session-1");
  for (const overrides of [
    { kind: "run_started", runSequence: "3", replayable: true },
    { kind: "assistant_delta", runSequence: "3", text: "early", livePreview: preview("attempt-1", "1", "3") },
    { kind: "run_cancelled", runSequence: "4", replayable: true },
    { kind: "assistant_delta", runSequence: "4", text: "too late", livePreview: preview("attempt-1", "2", "4") },
  ] satisfies Partial<TimelineEvent>[]) {
    state = reduceLiveTimelineEvent(state, timingEvent(workspaceId, runId, overrides));
    observeCommittedRunTimings(workspaceId, state);
  }
  bindSubmissionRun(submission, runId);
  observeRunTiming(workspaceId, runId, "first_content");
  expect((await rendererRunTimings(workspaceId)).map((entry) => entry.phase)).toEqual(["input_accepted", "admission"]);
});

test("committed cancellation settles once and terminal runs never gain late content", async () => {
  vi.stubGlobal("crypto", webcrypto);
  const workspaceId = "settled-workspace";
  const runId = "settled-run";
  const clock = vi.spyOn(performance, "now").mockReturnValue(100);
  bindSubmissionRun(beginSubmissionTiming(workspaceId), runId);
  observeRunTiming(workspaceId, runId, "cancellation_requested");
  clock.mockReturnValue(125);
  const state = reduceLiveTimelineEvent(createLiveEventState("session-1"),
    timingEvent(workspaceId, runId, { kind: "run_interrupted", runSequence: "4", replayable: true }));
  observeCommittedRunTimings(workspaceId, state);
  clock.mockReturnValue(150);
  observeCommittedRunTimings(workspaceId, state);
  observeRunTiming(workspaceId, runId, "first_content");
  const timings = await rendererRunTimings(workspaceId);
  expect(timings.filter((entry) => entry.phase === "cancellation_settled")).toMatchObject([{ elapsedUs: 25_000 }]);
  expect(timings.some((entry) => entry.phase === "first_content")).toBe(false);
});

function timingEvent(workspaceId: string, runId: string, overrides: Partial<TimelineEvent>): TimelineEvent {
  return { workspaceId, runId, sessionId: "session-1", sequence: 0, runSequence: "0", replayable: false, kind: "other", ...overrides };
}

function preview(attemptId: string, revision: string, baseSequence: string) {
  return { attemptId, revision, baseSequence, slotId: "text", truncated: false };
}
