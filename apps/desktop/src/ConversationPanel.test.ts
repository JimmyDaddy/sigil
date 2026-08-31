import { describe, expect, it } from "vitest";

import {
  terminalObservationFromRun,
  toContinuityPage,
} from "./ConversationPanel";
import {
  createConversationContinuityState,
  reduceConversationContinuity,
  type ConversationTerminalObservation,
} from "./features/conversation/continuityReducer";
import { terminalSignalFromTimelineEvent } from "./features/conversation/liveEventReducer";
import type { ConversationDisplayPage, RunSummary, TimelineEvent } from "./types";

const SESSION_ID = "conversation-panel-terminal-status";
const RUN_ID = "run-terminal-status";

describe("ConversationPanel terminal status projection", () => {
  it("accepts paused, blocked, and awaiting-user-input canonical frontiers", () => {
    for (const status of ["paused", "blocked", "awaiting_user_input"] as const) {
      expect(toContinuityPage(displayPage(status)).terminalFrontier).toEqual({
        runId: RUN_ID,
        sessionStreamSequence: "7",
        status,
      });
    }
  });

  it("treats the ambiguous paused snapshot as terminal transport while preserving blocked", () => {
    expect(terminalObservationFromRun(run("paused"))).toBeUndefined();
    expect(terminalObservationFromRun(run("blocked"))).toEqual({
      runId: RUN_ID,
      status: "blocked",
    });
  });

  it("settles paused and blocked snapshots after canonical refresh", () => {
    for (const status of ["paused", "blocked"] as const) {
      let state = reduceConversationContinuity(
        createConversationContinuityState(SESSION_ID),
        {
          type: "initial_page_received",
          sessionId: SESSION_ID,
          page: toContinuityPage(displayPage()),
        },
      );
      const terminal = terminalObservationFromRun(run(status));
      state = terminal === undefined
        ? reduceConversationContinuity(state, {
          type: "terminal_transport_observed",
          sessionId: SESSION_ID,
          runId: RUN_ID,
        })
        : reduceConversationContinuity(state, {
          type: "terminal_observed",
          sessionId: SESSION_ID,
          terminal,
        });
      expect(state.lifecycle).toBe("finalizing");

      state = reduceConversationContinuity(state, {
        type: "refresh_page_received",
        sessionId: SESSION_ID,
        page: toContinuityPage(displayPage(status)),
      });
      expect(state.lifecycle).toBe("idle");
      expect(state.canonicalTerminal).toEqual({
        runId: RUN_ID,
        sessionStreamSequence: "7",
        status,
      });
      expect(state.observedTerminal).toBeUndefined();
    }
  });

  it("settles awaiting-user-input after its user-input event and paused attach snapshot", () => {
    let state = reduceConversationContinuity(
      createConversationContinuityState(SESSION_ID),
      {
        type: "initial_page_received",
        sessionId: SESSION_ID,
        page: toContinuityPage(displayPage()),
      },
    );
    // `RunAwaitingUserInput` crosses the desktop bridge as the bounded request lifecycle
    // (`user_input_changed` / `requested`), not as a synthetic exact terminal signal. The
    // canonical display frontier remains the source for the exact AwaitingUserInput outcome.
    expect(terminalSignalFromTimelineEvent(awaitingUserInputEvent())).toBeUndefined();

    // `attachRun` ingests replayed events before observing the HTTP RunStatus snapshot.
    expect(terminalObservationFromRun(run("paused"))).toBeUndefined();
    state = reduceConversationContinuity(state, {
      type: "terminal_transport_observed",
      sessionId: SESSION_ID,
      runId: RUN_ID,
    });
    expect(state.pendingTerminalRunId).toBe(RUN_ID);
    expect(state.observedTerminal).toBeUndefined();
    expect(state.contractError).toBeUndefined();

    state = reduceConversationContinuity(state, {
      type: "refresh_page_received",
      sessionId: SESSION_ID,
      page: toContinuityPage(displayPage("awaiting_user_input")),
    });
    expect(state.lifecycle).toBe("idle");
    expect(state.canonicalTerminal?.status).toBe("awaiting_user_input");
  });
});

function displayPage(
  status?: ConversationTerminalObservation["status"],
): ConversationDisplayPage {
  return {
    schemaVersion: 1,
    requestScope: SESSION_ID,
    throughSessionStreamSequence: status === undefined ? "0" : "7",
    totalItems: "0",
    items: [],
    terminalFrontier: status === undefined
      ? undefined
      : {
        runId: RUN_ID,
        sessionStreamSequence: "7",
        status,
      },
    hasMore: false,
    gapFacts: [],
  };
}

function run(status: "paused" | "blocked"): RunSummary {
  return {
    id: RUN_ID,
    sessionId: SESSION_ID,
    status,
    permissionMode: "manual",
    streamSequence: 7,
  };
}

function awaitingUserInputEvent(): TimelineEvent {
  return {
    workspaceId: "workspace-terminal-status",
    sessionId: SESSION_ID,
    runId: RUN_ID,
    sequence: 7,
    runSequence: "7",
    replayable: true,
    kind: "user_input_changed",
    itemId: "user-input-terminal-status",
    status: "requested",
  };
}
