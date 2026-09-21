import { describe, expect, it } from "vitest";

import type { TimelineEvent } from "../../types";
import { mergeTaskProductProjections, projectCurrentTask } from "./taskProjection";

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

  it("projects direct execution and a real display checklist without inventing a plan step", () => {
    const task = projectCurrentTask([
      event("task_run_started", {
        task: { taskId: "task-direct", objective: "Execute the approved objective" },
      }),
      event("task_execution_admitted", {
        status: "admitted",
        task: {
          taskId: "task-direct",
          execution: { kind: "direct", admissionId: "admission-direct" },
        },
      }),
      event("task_checklist_updated", {
        status: "updated",
        task: {
          taskId: "task-direct",
          checklistRevision: 2,
          checklist: [
            { itemId: "inspect", text: "Inspect", status: "completed" },
            { itemId: "implement", text: "Implement", status: "in_progress" },
            { itemId: "verify", text: "Verify independently", status: "in_progress" },
          ],
        },
      }),
    ]);

    expect(task).toMatchObject({
      taskId: "task-direct",
      execution: { kind: "direct", admissionId: "admission-direct" },
      steps: [],
      checklist: [
        { text: "Inspect", status: "completed" },
        { text: "Implement", status: "in_progress" },
        { text: "Verify independently", status: "in_progress" },
      ],
    });
    expect(task?.planVersion).toBeUndefined();
  });
});

describe("Task projection merge", () => {
  const savedItems = [{ itemId: "inspect", text: "Inspect", status: "completed" as const }];
  const durableTask = projectCurrentTask([
    event("task_checklist_updated", {
      task: { taskId: "task-1", checklistRevision: 1, checklist: savedItems },
    }),
    event("task_run_started", {
      task: { taskId: "task-1", objective: "Implement the change" },
    }),
  ]);

  it("keeps an explicit streamed clear instead of restoring the saved checklist", () => {
    const streamedTask = projectCurrentTask([
      event("task_checklist_updated", {
        task: { taskId: "task-1", checklistRevision: 2, checklist: [] },
      }),
    ]);
    expect(mergeTaskProductProjections(durableTask, streamedTask)).toMatchObject({
      taskId: "task-1",
      objective: "Implement the change",
      checklist: [],
    });
  });

  it("keeps the saved checklist when streamed task events contain no checklist update", () => {
    const streamedTask = projectCurrentTask([
      event("task_phase_changed", {
        status: "running",
        task: { taskId: "task-1", phase: "execution" },
      }),
    ]);
    expect(mergeTaskProductProjections(durableTask, streamedTask)).toMatchObject({
      taskId: "task-1",
      checklist: savedItems,
    });
    expect(mergeTaskProductProjections(durableTask, undefined)).toBe(durableTask);
  });

  it("replaces saved items with a streamed nonempty update", () => {
    const items = [{ itemId: "verify", text: "Verify", status: "in_progress" as const }];
    const streamedTask = projectCurrentTask([
      event("task_checklist_updated", {
        task: { taskId: "task-1", checklistRevision: 2, checklist: items },
      }),
    ]);
    expect(mergeTaskProductProjections(durableTask, streamedTask)?.checklist).toEqual(items);
  });

  it("does not carry the saved checklist into a different task", () => {
    const streamedTask = projectCurrentTask([
      event("task_run_started", {
        task: { taskId: "task-2", objective: "A different objective" },
      }),
    ]);
    expect(mergeTaskProductProjections(durableTask, streamedTask)).toBe(streamedTask);
    expect(mergeTaskProductProjections(undefined, streamedTask)).toBe(streamedTask);
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
