import { describe, expect, it } from "vitest";

import { translateEnglish } from "./i18n";
import {
  presentArtifactStatus,
  presentCleanupStatus,
  presentComposerActivity,
  presentDeliveryStatus,
  presentProductStatus,
  presentRunStatus,
  presentTaskStatus,
  presentTerminalTask,
  type ProductStatus,
} from "./statusPresentation";
import type {
  RunStatus,
  TaskExecutionPhase,
  TimelineTerminalTask,
  ToolArtifactAvailability,
} from "./types";

const runStatusMappings: ReadonlyArray<readonly [RunStatus, ProductStatus]> = [
  ["starting", "loading"],
  ["running", "running"],
  ["waiting_for_approval", "waiting"],
  ["cancel_requested", "waiting"],
  ["pause_requested", "waiting"],
  ["execution_uncertain", "uncertain"],
  ["finished", "completed"],
  ["failed", "failed"],
  ["cancelled", "cancelled"],
  ["paused", "paused"],
  ["blocked", "blocked"],
  ["interrupted", "interrupted"],
];

const taskPhases: readonly TaskExecutionPhase[] = [
  "preparing",
  "ready",
  "running",
  "blocked",
  "paused",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
];

const artifactStatuses: readonly ToolArtifactAvailability[] = [
  "available",
  "expired",
  "missing",
  "hash_mismatch",
  "policy_revoked",
  "unavailable",
];

function terminalTask(
  status: TimelineTerminalTask["status"],
  overrides: Partial<TimelineTerminalTask> = {},
): TimelineTerminalTask {
  return {
    taskId: "terminal-1",
    generation: 1,
    status,
    readiness: "none",
    totalOutputBytes: 0,
    emittedAtMs: 1,
    ...overrides,
  };
}

function expectCompletePresentation(presentation: ReturnType<typeof presentProductStatus>) {
  expect(presentation.label).not.toBe("");
  expect(presentation.message).not.toBe("");
  expect(presentation.nextStep).not.toBe("");
  expect(presentation.technicalDetail).not.toBe("");
}

describe("typed product status presentation", () => {
  it("covers every run status without collapsing requests or uncertainty", () => {
    for (const [runStatus, productStatus] of runStatusMappings) {
      const presentation = presentRunStatus(runStatus, translateEnglish);
      expect(presentation.status).toBe(productStatus);
      expectCompletePresentation(presentation);
    }

    const uncertain = presentRunStatus("execution_uncertain", translateEnglish);
    expect(uncertain.status).toBe("uncertain");
    expect(uncertain.label).toBe("Needs confirmation");
    expect(uncertain.technicalDetail).toContain("do not assume success or cancellation");

    const stopRequested = presentRunStatus("cancel_requested", translateEnglish);
    expect(stopRequested.status).toBe("waiting");
    expect(stopRequested.label).not.toBe("Cancelled");
    expect(stopRequested.message).toContain("cancellation is not confirmed");
  });

  it("maps task phases and unknown wire values to explicit safe product states", () => {
    for (const phase of taskPhases) {
      expectCompletePresentation(presentTaskStatus(phase, translateEnglish));
    }

    expect(presentTaskStatus("ready", translateEnglish).status).toBe("waiting");
    const unknown = presentTaskStatus("wire_value_added_later", translateEnglish);
    expect(unknown.status).toBe("uncertain");
    expect(unknown.technicalDetail).toContain("wire_value_added_later");
    expect(unknown.label).not.toBe("Completed");
    expect(unknown.label).not.toBe("Cancelled");
  });

  it("keeps terminal process readiness and exit evidence distinct", () => {
    expect(presentTerminalTask(terminalTask("starting"), translateEnglish).status).toBe("loading");
    expect(
      presentTerminalTask(terminalTask("running", { readiness: "waiting" }), translateEnglish).status,
    ).toBe("waiting");
    expect(
      presentTerminalTask(terminalTask("running", { readiness: "ready" }), translateEnglish).status,
    ).toBe("running");
    expect(
      presentTerminalTask(terminalTask("running", { readiness: "failed" }), translateEnglish).status,
    ).toBe("blocked");
    expect(
      presentTerminalTask(terminalTask("running", { readiness: "timed_out" }), translateEnglish).status,
    ).toBe("blocked");
    expect(
      presentTerminalTask(terminalTask("exited", { exitCode: 0 }), translateEnglish).status,
    ).toBe("completed");
    expect(
      presentTerminalTask(terminalTask("exited", { exitCode: 1 }), translateEnglish).status,
    ).toBe("failed");
    const missingExitCode = presentTerminalTask(terminalTask("exited"), translateEnglish);
    expect(missingExitCode.status).toBe("uncertain");
    expect(missingExitCode.label).not.toBe("Completed");
    expect(missingExitCode.technicalDetail).toContain("not recorded");
    expect(presentTerminalTask(terminalTask("failed"), translateEnglish).status).toBe("failed");
    expect(presentTerminalTask(terminalTask("cancelled"), translateEnglish).status).toBe("cancelled");
    expect(presentTerminalTask(terminalTask("interrupted"), translateEnglish).status).toBe("interrupted");

    const exited = presentTerminalTask(terminalTask("exited", { exitCode: 0 }), translateEnglish);
    expect(exited.nextStep).toContain("does not prove Task success");
    expect(exited.technicalDetail).toContain("independent of Task completion");
  });

  it("keeps composer activity copy on the typed product status path", () => {
    const running = presentComposerActivity("running", translateEnglish);
    expect(running.status).toBe("running");
    expect(running.action).toBe("stop");
    expect(running.label).toBe("Sigil is working");
    expect(running.message).toContain("Live output is updating");

    const reconnecting = presentComposerActivity("reconnecting", translateEnglish);
    expect(reconnecting.status).toBe("uncertain");
    expect(reconnecting.action).toBe("reconcile");
  });

  it("keeps artifact, cleanup, and delivery states independent from task outcome", () => {
    expect(presentArtifactStatus("available", translateEnglish).status).toBe("completed");
    for (const status of artifactStatuses.slice(1)) {
      const presentation = presentArtifactStatus(status, translateEnglish);
      expect(presentation.status).toBe("blocked");
      expect(presentation.technicalDetail).toContain(status);
      expect(presentation.nextStep).toContain("do not treat this as an execution failure");
    }

    expect(presentCleanupStatus([], translateEnglish)).toBeUndefined();
    const cleanup = presentCleanupStatus(["first", "second"], translateEnglish);
    expect(cleanup?.status).toBe("blocked");
    expect(cleanup?.technicalDetail).toContain("2 error(s)");

    expect(presentDeliveryStatus("pending", translateEnglish).status).toBe("waiting");
    expect(presentDeliveryStatus("accepted", translateEnglish).status).toBe("waiting");
    expect(presentDeliveryStatus("delivery_uncertain", translateEnglish).status).toBe("uncertain");
    expect(presentDeliveryStatus("accepted", translateEnglish).technicalDetail).toContain("not execution success");
  });

  it("provides complete copy for every product state", () => {
    const statuses: readonly ProductStatus[] = [
      "loading",
      "running",
      "waiting",
      "paused",
      "blocked",
      "uncertain",
      "failed",
      "cancelled",
      "interrupted",
      "completed",
    ];
    for (const status of statuses) {
      expectCompletePresentation(presentProductStatus(status, translateEnglish));
    }
  });
});
