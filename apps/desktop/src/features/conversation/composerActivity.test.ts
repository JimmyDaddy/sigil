import { describe, expect, it } from "vitest";

import { resolveComposerActivityState } from "./composerActivity";

const idleSignals = {
  active: false,
  submitting: false,
  controlBusy: false,
  approvalPending: false,
  runStatus: undefined,
  streamState: undefined,
  continuityLifecycle: "idle" as const,
};

describe("composer activity state", () => {
  it("keeps an active task visible beside the composer", () => {
    expect(resolveComposerActivityState({
      ...idleSignals,
      active: true,
      runStatus: "running",
      streamState: "live",
    })).toBe("running");
  });

  it("prioritizes states that require the user's attention or explain missing updates", () => {
    expect(resolveComposerActivityState({
      ...idleSignals,
      active: true,
      approvalPending: true,
      runStatus: "running",
      streamState: "live",
    })).toBe("waiting_for_approval");
    expect(resolveComposerActivityState({
      ...idleSignals,
      active: true,
      runStatus: "running",
      streamState: "reconnecting",
    })).toBe("reconnecting");
  });

  it("does not add status noise while the conversation is idle", () => {
    expect(resolveComposerActivityState(idleSignals)).toBeUndefined();
  });
});
