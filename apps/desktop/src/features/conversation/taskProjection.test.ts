import { describe, expect, it } from "vitest";

import type { TimelineEvent } from "../../types";
import { projectCurrentTask } from "./taskProjection";

describe("Task product projection", () => {
  it("combines the typed plan, step, batch, and integration lane slots", () => {
    const task = projectCurrentTask([
      event("task_run_started", {
        status: "running",
        task: { taskId: "task-1", objective: "Implement the control plane" },
      }),
      event("task_phase_changed", {
        status: "running",
        task: { taskId: "task-1", phase: "integration" },
      }),
      event("task_plan_updated", {
        status: "approved",
        task: {
          taskId: "task-1",
          planVersion: 3,
          steps: [{
            stepId: "step-1",
            title: "Inspect",
            role: "explorer",
            dependsOn: [],
            mode: "read_only",
            isolation: "shared",
          }],
        },
      }),
      event("task_step_changed", {
        status: "completed",
        task: { taskId: "task-1", planVersion: 3, stepId: "step-1" },
      }),
      event("task_batch_changed", {
        task: {
          taskId: "task-1",
          planVersion: 3,
          batchId: "batch-1",
          active: 0,
          completed: 1,
          failed: 0,
        },
      }),
      event("integration_lane_changed", {
        status: "ready",
        task: {
          taskId: "task-1",
          planVersion: 3,
          planId: "plan-1",
          laneId: "lane-1",
          conflicts: ["path_overlap"],
        },
      }),
    ]);

    expect(task).toMatchObject({
      taskId: "task-1",
      objective: "Implement the control plane",
      phase: "integration",
      status: "running",
      planVersion: 3,
      planStatus: "approved",
      completedChildren: 1,
      failedChildren: 0,
      canContinue: true,
      steps: [{ stepId: "step-1", status: "completed" }],
      lanes: [{ laneId: "lane-1", status: "ready", conflicts: ["path_overlap"] }],
    });
  });

  it("keeps interrupted Tasks continuable but closes completed and cancelled Tasks", () => {
    for (const [status, canContinue] of [
      ["interrupted", true],
      ["failed", true],
      ["paused", true],
      ["completed", false],
      ["cancelled", false],
    ] as const) {
      const task = projectCurrentTask([
        event("task_run_started", {
          task: { taskId: "task-1", objective: "Resume safely" },
        }),
        event("task_run_finished", {
          status,
          task: { taskId: "task-1" },
        }),
      ]);
      expect(task?.canContinue, status).toBe(canContinue);
    }
  });
});

function event(
  kind: TimelineEvent["kind"],
  overrides: Partial<TimelineEvent>,
): TimelineEvent {
  return {
    workspaceId: "workspace-1",
    sessionId: "session-1",
    runId: "run-1",
    sequence: 1,
    runSequence: "1",
    replayable: true,
    kind,
    ...overrides,
  };
}
