import type {
  TaskChecklistItem,
  TaskExecutionBinding,
  TimelineEvent,
  TimelineTaskPhase,
  TimelineTaskPlanStep,
} from "../../types";

export interface TaskStepProjection extends TimelineTaskPlanStep {
  status?: string;
}

export interface TaskLaneProjection {
  laneId: string;
  planId?: string;
  status?: string;
  conflicts: string[];
}

export interface TaskProductProjection {
  taskId: string;
  objective?: string;
  phase?: TimelineTaskPhase;
  status: string;
  execution?: TaskExecutionBinding;
  planVersion?: number;
  planStatus?: string;
  steps: TaskStepProjection[];
  checklist: TaskChecklistItem[];
  /** A streamed checklist replacement was observed, including an explicit empty list. */
  hasChecklistUpdate?: boolean;
  activeChildren: number;
  completedChildren: number;
  failedChildren: number;
  lanes: TaskLaneProjection[];
  canContinue: boolean;
}

const NON_CONTINUABLE_TASK_STATUSES = new Set(["completed", "cancelled"]);

export function projectCurrentTask(
  events: readonly TimelineEvent[],
): TaskProductProjection | undefined {
  const taskId = events.find((event) => event.task?.taskId !== undefined)?.task?.taskId;
  if (taskId === undefined) return undefined;
  const taskEvents = events.filter((event) => event.task?.taskId === taskId);
  const started = findKind(taskEvents, "task_run_started");
  const finished = findKind(taskEvents, "task_run_finished");
  const phase = findKind(taskEvents, "task_phase_changed");
  const execution = findKind(taskEvents, "task_execution_admitted");
  const plan = findKind(taskEvents, "task_plan_updated");
  const checklist = findKind(taskEvents, "task_checklist_updated");
  const stepStatuses = new Map(
    taskEvents
      .filter((event) => event.kind === "task_step_changed" && event.task?.stepId !== undefined)
      .map((event) => [event.task?.stepId ?? "", event.status]),
  );
  const plannedSteps = plan?.task?.steps ?? [];
  const steps = plannedSteps.map((step) => ({
    ...step,
    status: stepStatuses.get(step.stepId),
  }));
  for (const [stepId, status] of stepStatuses) {
    if (steps.some((step) => step.stepId === stepId)) continue;
    steps.push({
      stepId,
      title: stepId,
      role: "unknown",
      dependsOn: [],
      mode: "unknown",
      isolation: "unknown",
      status,
    });
  }
  const batches = taskEvents.filter((event) => event.kind === "task_batch_changed");
  const lanes = taskEvents
    .filter((event) => event.kind === "integration_lane_changed")
    .map((event) => ({
      laneId: event.task?.laneId ?? "",
      planId: event.task?.planId,
      status: event.status,
      conflicts: event.task?.conflicts ?? [],
    }))
    .filter((lane) => lane.laneId !== "");
  const status = finished?.status ?? phase?.status ?? (started === undefined ? "unknown" : "running");

  return {
    taskId,
    objective: started?.task?.objective,
    phase: phase?.task?.phase,
    status,
    execution: execution?.task?.execution,
    planVersion: plan?.task?.planVersion ?? phase?.task?.planVersion,
    planStatus: plan?.status,
    steps,
    checklist: checklist?.task?.checklist ?? [],
    hasChecklistUpdate: checklist?.task?.checklist !== undefined,
    activeChildren: sumTaskCount(batches, "active"),
    completedChildren: sumTaskCount(batches, "completed"),
    failedChildren: sumTaskCount(batches, "failed"),
    lanes,
    canContinue: !NON_CONTINUABLE_TASK_STATUSES.has(status.toLowerCase()),
  };
}

export function mergeTaskProductProjections(
  durableTask: TaskProductProjection | undefined,
  eventTask: TaskProductProjection | undefined,
): TaskProductProjection | undefined {
  if (eventTask === undefined) return durableTask;
  if (durableTask === undefined || durableTask.taskId !== eventTask.taskId) return eventTask;
  return {
    ...durableTask,
    ...eventTask,
    objective: eventTask.objective ?? durableTask.objective,
    execution: eventTask.execution ?? durableTask.execution,
    steps: eventTask.steps.length === 0 ? durableTask.steps : eventTask.steps,
    checklist: eventTask.hasChecklistUpdate || eventTask.checklist.length > 0
      ? eventTask.checklist
      : durableTask.checklist,
  };
}

function findKind(
  events: readonly TimelineEvent[],
  kind: TimelineEvent["kind"],
): TimelineEvent | undefined {
  return events.find((event) => event.kind === kind);
}

function sumTaskCount(
  events: readonly TimelineEvent[],
  key: "active" | "completed" | "failed",
): number {
  return events.reduce((total, event) => total + (event.task?.[key] ?? 0), 0);
}
