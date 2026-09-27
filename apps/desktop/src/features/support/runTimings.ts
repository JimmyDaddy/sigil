import type { LiveEventState } from "../conversation/liveEventReducer";

/** Volatile timing only: never persist prompts, content, paths, or authorization state. */
export type RendererTimingPhase = "input_accepted" | "first_feedback_frame" | "admission" | "first_content" | "cancellation_requested" | "cancellation_settled";
export interface RendererRunTiming {
  readonly submissionKey: string;
  readonly runKey: string;
  readonly phase: RendererTimingPhase;
  readonly elapsedUs: number;
}

interface SubmissionTiming {
  readonly workspaceId: string;
  readonly localId: string;
  readonly started: number;
  runId?: string;
  retired?: boolean;
  readonly phases: Map<RendererTimingPhase, number>;
}

const submissions: SubmissionTiming[] = [];
const MAX_SUBMISSIONS = 32;
const earlyContent = new Map<string, number>();
const retiredRuns = new Set<string>();
let nextSubmission = 0;

export function beginSubmissionTiming(workspaceId: string): SubmissionTiming {
  const timing: SubmissionTiming = {
    workspaceId, localId: `renderer:${performance.timeOrigin}:${++nextSubmission}`, started: performance.now(),
    phases: new Map([["input_accepted", 0]]),
  };
  submissions.push(timing);
  if (submissions.length > MAX_SUBMISSIONS) submissions.shift();
  return timing;
}

/** Call from a committed feedback render. A second frame observes the preceding paint. */
export function observeFeedbackFrame(timing: SubmissionTiming): () => void {
  let second: number | undefined;
  const first = requestAnimationFrame(() => {
    second = requestAnimationFrame(() => markSubmissionTiming(timing, "first_feedback_frame"));
  });
  return () => {
    cancelAnimationFrame(first);
    if (second !== undefined) cancelAnimationFrame(second);
  };
}

export function bindSubmissionRun(timing: SubmissionTiming, runId: string): void {
    timing.runId = runId;
    markSubmissionTiming(timing, "admission");
    const key = `${timing.workspaceId}:${runId}`;
    timing.retired = retiredRuns.has(key);
    const arrived = timing.retired ? undefined : earlyContent.get(key);
    if (arrived !== undefined) {
      timing.phases.set("first_content", Math.max(0, Math.round((arrived - timing.started) * 1_000)));
    }
    earlyContent.delete(key);
}

function markSubmissionTiming(timing: SubmissionTiming, phase: RendererTimingPhase): void {
  if (!timing.phases.has(phase)) {
    timing.phases.set(phase, Math.max(0, Math.round((performance.now() - timing.started) * 1_000)));
  }
}

export function observeRunTiming(workspaceId: string, runId: string, phase: RendererTimingPhase): void {
  if (retiredRuns.has(`${workspaceId}:${runId}`)) return;
  const timing = [...submissions].reverse().find((item) => item.workspaceId === workspaceId && item.runId === runId);
  if (timing !== undefined) {
    if (timing.retired) return;
    if (phase !== "cancellation_settled" || timing.phases.has("cancellation_requested")) markSubmissionTiming(timing, phase);
  } else if (phase === "first_content") {
    const key = `${workspaceId}:${runId}`;
    if (!earlyContent.has(key)) earlyContent.set(key, performance.now());
    if (earlyContent.size > MAX_SUBMISSIONS) earlyContent.delete(earlyContent.keys().next().value!);
  }
}

/** Observe only committed reducer output; rejected previews never become timing facts. */
export function observeCommittedRunTimings(workspaceId: string, state: LiveEventState): void {
  for (const buffer of state.deltaBuffers.values()) {
    if ([...buffer.fragments.values()].some((text) => text.length > 0)) {
      observeRunTiming(workspaceId, buffer.runId, "first_content");
    }
  }
  for (const item of state.semanticItems.values()) {
    if ((item.content.type === "reasoning"
      || item.content.type === "message" && item.content.role === "assistant")
      && (item.content.text?.length ?? 0) > 0) {
      observeRunTiming(workspaceId, item.runId, "first_content");
    }
  }
  for (const terminal of state.terminalSignals.values()) {
    if (terminal.status === "cancelled" || terminal.status === "interrupted") {
      observeRunTiming(workspaceId, terminal.runId, "cancellation_settled");
    }
    retireRunTiming(workspaceId, terminal.runId);
  }
}

export function retireRunTiming(workspaceId: string, runId: string): void {
  const key = `${workspaceId}:${runId}`;
  earlyContent.delete(key);
  retiredRuns.add(key);
  if (retiredRuns.size > MAX_SUBMISSIONS) retiredRuns.delete(retiredRuns.values().next().value!);
  for (const timing of submissions) {
    if (timing.workspaceId === workspaceId && timing.runId === runId) timing.retired = true;
  }
}

/** Hash host run IDs using the same label hash as native timings, only during explicit export. */
export async function rendererRunTimings(workspaceId: string): Promise<RendererRunTiming[]> {
  const selected = submissions.filter((item) => item.workspaceId === workspaceId)
    .map((item) => ({ localId: item.localId, runId: item.runId, phases: Array.from(item.phases) }));
  try {
    return (await Promise.all(selected.map(async (item) => {
      const [runKey, submissionKey] = await Promise.all([
        timingKey(item.runId ?? item.localId), timingKey(item.localId),
      ]);
      return item.phases.map(([phase, elapsedUs]) => ({ runKey, submissionKey, phase, elapsedUs }));
    }))).flat();
  } catch {
    // An unavailable WebCrypto diagnostic must not prevent the server support export.
    return [];
  }
}

async function timingKey(identity: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(identity));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
