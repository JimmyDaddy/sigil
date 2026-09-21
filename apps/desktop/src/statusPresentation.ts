import type { Translate } from "./i18n";
import type { ComposerActivityState } from "./features/conversation/composerActivity";
import type {
  RunStatus,
  TaskExecutionPhase,
  TerminalReadinessStatus,
  TimelineTerminalTask,
  ToolArtifactAvailability,
} from "./types";

export type ProductStatus =
  | "loading"
  | "running"
  | "waiting"
  | "paused"
  | "blocked"
  | "uncertain"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "completed";

export type TaskExecutionDisplayPhase =
  | TaskExecutionPhase
  | "preparing"
  | "blocked";

export type ProductStatusAction =
  | "wait"
  | "stop"
  | "review_approval"
  | "reconcile"
  | "review"
  | "continue"
  | "start_new";

export interface ProductStatusPresentation {
  readonly status: ProductStatus;
  readonly action: ProductStatusAction;
  readonly label: string;
  readonly message: string;
  readonly nextStep: string;
  readonly technicalDetail: string;
}

type StatusCopy = {
  readonly status: ProductStatus;
  readonly action: ProductStatusAction;
  readonly labelKey: StatusLabelKey;
  readonly messageKey: StatusMessageKey;
  readonly nextStepKey: StatusNextStepKey;
  readonly technicalDetailKey: StatusDetailKey;
  readonly values?: Record<string, string | number>;
};

type StatusLabelKey =
  | "productStatusLabel_loading"
  | "productStatusLabel_running"
  | "productStatusLabel_waiting"
  | "productStatusLabel_paused"
  | "productStatusLabel_blocked"
  | "productStatusLabel_uncertain"
  | "productStatusLabel_failed"
  | "productStatusLabel_cancelled"
  | "productStatusLabel_interrupted"
  | "productStatusLabel_completed"
  | "composerActivityStarting"
  | "composerActivityConnecting"
  | "composerActivityRunning"
  | "composerActivityApproval"
  | "composerActivityStopping"
  | "composerActivityRecovering"
  | "composerActivityReconnecting"
  | "composerActivityConnectionError"
  | "composerActivityFinalizing";

type StatusMessageKey =
  | "productStatusMessage_loading"
  | "productStatusMessage_running"
  | "productStatusMessage_waiting"
  | "productStatusMessage_paused"
  | "productStatusMessage_blocked"
  | "productStatusMessage_uncertain"
  | "productStatusMessage_failed"
  | "productStatusMessage_cancelled"
  | "productStatusMessage_interrupted"
  | "productStatusMessage_completed"
  | "runStatusMessage_approval"
  | "runStatusMessage_stopRequested"
  | "runStatusMessage_pauseRequested"
  | "taskStatusMessage_ready"
  | "terminalTaskMessage_readinessWaiting"
  | "terminalTaskMessage_readinessBlocked"
  | "terminalTaskMessage_exited"
  | "terminalTaskMessage_exitFailed"
  | "artifactStatusMessage_available"
  | "artifactStatusMessage_unavailable"
  | "cleanupStatusMessage_attention"
  | "deliveryStatusMessage_accepted"
  | "composerActivityStartingDetail"
  | "composerActivityConnectingDetail"
  | "composerActivityRunningDetail"
  | "composerActivityApprovalDetail"
  | "composerActivityStoppingDetail"
  | "composerActivityRecoveringDetail"
  | "composerActivityReconnectingDetail"
  | "composerActivityConnectionErrorDetail"
  | "composerActivityFinalizingDetail";

type StatusNextStepKey =
  | "productStatusNext_loading"
  | "productStatusNext_running"
  | "productStatusNext_waiting"
  | "productStatusNext_paused"
  | "productStatusNext_blocked"
  | "productStatusNext_uncertain"
  | "productStatusNext_failed"
  | "productStatusNext_cancelled"
  | "productStatusNext_interrupted"
  | "productStatusNext_completed"
  | "runStatusNext_approval"
  | "runStatusNext_stopRequested"
  | "runStatusNext_pauseRequested"
  | "taskStatusNext_ready"
  | "terminalTaskNext_readinessWaiting"
  | "terminalTaskNext_readinessBlocked"
  | "terminalTaskNext_exited"
  | "terminalTaskNext_exitFailed"
  | "artifactStatusNext_available"
  | "artifactStatusNext_unavailable"
  | "cleanupStatusNext_attention"
  | "deliveryStatusNext_accepted";

type StatusDetailKey =
  | "productStatusDetail_loading"
  | "productStatusDetail_running"
  | "productStatusDetail_waiting"
  | "productStatusDetail_paused"
  | "productStatusDetail_blocked"
  | "productStatusDetail_uncertain"
  | "productStatusDetail_failed"
  | "productStatusDetail_cancelled"
  | "productStatusDetail_interrupted"
  | "productStatusDetail_completed"
  | "runStatusDetail_approval"
  | "runStatusDetail_stopRequested"
  | "runStatusDetail_pauseRequested"
  | "taskStatusDetail_ready"
  | "taskStatusDetail_unrecognized"
  | "terminalTaskDetail_readinessWaiting"
  | "terminalTaskDetail_readinessBlocked"
  | "terminalTaskDetail_exited"
  | "terminalTaskDetail_exitFailed"
  | "artifactStatusDetail_available"
  | "artifactStatusDetail_unavailable"
  | "cleanupStatusDetail_attention"
  | "deliveryStatusDetail_accepted";

const PRODUCT_STATUS_COPY: Record<ProductStatus, StatusCopy> = {
  loading: {
    status: "loading",
    action: "wait",
    labelKey: "productStatusLabel_loading",
    messageKey: "productStatusMessage_loading",
    nextStepKey: "productStatusNext_loading",
    technicalDetailKey: "productStatusDetail_loading",
  },
  running: {
    status: "running",
    action: "stop",
    labelKey: "productStatusLabel_running",
    messageKey: "productStatusMessage_running",
    nextStepKey: "productStatusNext_running",
    technicalDetailKey: "productStatusDetail_running",
  },
  waiting: {
    status: "waiting",
    action: "wait",
    labelKey: "productStatusLabel_waiting",
    messageKey: "productStatusMessage_waiting",
    nextStepKey: "productStatusNext_waiting",
    technicalDetailKey: "productStatusDetail_waiting",
  },
  paused: {
    status: "paused",
    action: "continue",
    labelKey: "productStatusLabel_paused",
    messageKey: "productStatusMessage_paused",
    nextStepKey: "productStatusNext_paused",
    technicalDetailKey: "productStatusDetail_paused",
  },
  blocked: {
    status: "blocked",
    action: "review",
    labelKey: "productStatusLabel_blocked",
    messageKey: "productStatusMessage_blocked",
    nextStepKey: "productStatusNext_blocked",
    technicalDetailKey: "productStatusDetail_blocked",
  },
  uncertain: {
    status: "uncertain",
    action: "reconcile",
    labelKey: "productStatusLabel_uncertain",
    messageKey: "productStatusMessage_uncertain",
    nextStepKey: "productStatusNext_uncertain",
    technicalDetailKey: "productStatusDetail_uncertain",
  },
  failed: {
    status: "failed",
    action: "review",
    labelKey: "productStatusLabel_failed",
    messageKey: "productStatusMessage_failed",
    nextStepKey: "productStatusNext_failed",
    technicalDetailKey: "productStatusDetail_failed",
  },
  cancelled: {
    status: "cancelled",
    action: "start_new",
    labelKey: "productStatusLabel_cancelled",
    messageKey: "productStatusMessage_cancelled",
    nextStepKey: "productStatusNext_cancelled",
    technicalDetailKey: "productStatusDetail_cancelled",
  },
  interrupted: {
    status: "interrupted",
    action: "continue",
    labelKey: "productStatusLabel_interrupted",
    messageKey: "productStatusMessage_interrupted",
    nextStepKey: "productStatusNext_interrupted",
    technicalDetailKey: "productStatusDetail_interrupted",
  },
  completed: {
    status: "completed",
    action: "review",
    labelKey: "productStatusLabel_completed",
    messageKey: "productStatusMessage_completed",
    nextStepKey: "productStatusNext_completed",
    technicalDetailKey: "productStatusDetail_completed",
  },
};

const COMPOSER_ACTIVITY_COPY: Record<ComposerActivityState, StatusCopy> = {
  starting: {
    ...PRODUCT_STATUS_COPY.loading,
    labelKey: "composerActivityStarting",
    messageKey: "composerActivityStartingDetail",
  },
  connecting: {
    ...PRODUCT_STATUS_COPY.loading,
    labelKey: "composerActivityConnecting",
    messageKey: "composerActivityConnectingDetail",
  },
  running: {
    ...PRODUCT_STATUS_COPY.running,
    labelKey: "composerActivityRunning",
    messageKey: "composerActivityRunningDetail",
  },
  waiting_for_approval: {
    ...PRODUCT_STATUS_COPY.waiting,
    action: "review_approval",
    labelKey: "composerActivityApproval",
    messageKey: "composerActivityApprovalDetail",
  },
  stopping: {
    ...PRODUCT_STATUS_COPY.waiting,
    action: "stop",
    labelKey: "composerActivityStopping",
    messageKey: "composerActivityStoppingDetail",
  },
  recovering: {
    ...PRODUCT_STATUS_COPY.uncertain,
    labelKey: "composerActivityRecovering",
    messageKey: "composerActivityRecoveringDetail",
  },
  reconnecting: {
    ...PRODUCT_STATUS_COPY.uncertain,
    labelKey: "composerActivityReconnecting",
    messageKey: "composerActivityReconnectingDetail",
  },
  connection_error: {
    ...PRODUCT_STATUS_COPY.uncertain,
    labelKey: "composerActivityConnectionError",
    messageKey: "composerActivityConnectionErrorDetail",
  },
  finalizing: {
    ...PRODUCT_STATUS_COPY.loading,
    labelKey: "composerActivityFinalizing",
    messageKey: "composerActivityFinalizingDetail",
  },
};

const RUN_STATUS_PRODUCT: Record<RunStatus, ProductStatus> = {
  starting: "loading",
  running: "running",
  waiting_for_approval: "waiting",
  cancel_requested: "waiting",
  pause_requested: "waiting",
  execution_uncertain: "uncertain",
  finished: "completed",
  failed: "failed",
  cancelled: "cancelled",
  paused: "paused",
  blocked: "blocked",
  interrupted: "interrupted",
};

export function presentProductStatus(
  status: ProductStatus,
  t: Translate,
): ProductStatusPresentation {
  return localizeStatus(PRODUCT_STATUS_COPY[status], t);
}

export function presentComposerActivity(
  state: ComposerActivityState,
  t: Translate,
): ProductStatusPresentation {
  return localizeStatus(COMPOSER_ACTIVITY_COPY[state], t);
}

export function presentRunStatus(
  status: RunStatus,
  t: Translate,
): ProductStatusPresentation {
  const productStatus = RUN_STATUS_PRODUCT[status];
  if (status === "waiting_for_approval") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY[productStatus],
      action: "review_approval",
      messageKey: "runStatusMessage_approval",
      nextStepKey: "runStatusNext_approval",
      technicalDetailKey: "runStatusDetail_approval",
    }, t);
  }
  if (status === "cancel_requested") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY[productStatus],
      messageKey: "runStatusMessage_stopRequested",
      nextStepKey: "runStatusNext_stopRequested",
      technicalDetailKey: "runStatusDetail_stopRequested",
    }, t);
  }
  if (status === "pause_requested") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY[productStatus],
      messageKey: "runStatusMessage_pauseRequested",
      nextStepKey: "runStatusNext_pauseRequested",
      technicalDetailKey: "runStatusDetail_pauseRequested",
    }, t);
  }
  return localizeStatus(PRODUCT_STATUS_COPY[productStatus], t);
}

export function presentTaskStatus(
  status: string,
  t: Translate,
): ProductStatusPresentation {
  switch (status) {
    case "preparing": return presentTaskExecutionPhase("preparing", t);
    case "ready": return presentReadyTaskStatus(t);
    case "running":
    case "started":
      return presentTaskExecutionPhase("running", t);
    case "blocked": return presentTaskExecutionPhase("blocked", t);
    case "paused": return presentTaskExecutionPhase("paused", t);
    case "completed": return presentTaskExecutionPhase("completed", t);
    case "failed": return presentTaskExecutionPhase("failed", t);
    case "cancelled": return presentTaskExecutionPhase("cancelled", t);
    case "interrupted": return presentTaskExecutionPhase("interrupted", t);
    default: return localizeStatus({
      ...PRODUCT_STATUS_COPY.uncertain,
      technicalDetailKey: "taskStatusDetail_unrecognized",
      values: { status },
    }, t);
  }
}

export function isTaskRunningStatus(status: string): boolean {
  return status === "started" || status === "running";
}

function presentTaskExecutionPhase(
  phase: TaskExecutionDisplayPhase,
  t: Translate,
): ProductStatusPresentation {
  switch (phase) {
    case "preparing": return presentProductStatus("loading", t);
    case "ready": return presentReadyTaskStatus(t);
    case "running": return presentProductStatus("running", t);
    case "blocked": return presentProductStatus("blocked", t);
    case "paused": return presentProductStatus("paused", t);
    case "completed": return presentProductStatus("completed", t);
    case "failed": return presentProductStatus("failed", t);
    case "cancelled": return presentProductStatus("cancelled", t);
    case "interrupted": return presentProductStatus("interrupted", t);
  }
}

function presentReadyTaskStatus(t: Translate): ProductStatusPresentation {
  return localizeStatus({
    ...PRODUCT_STATUS_COPY.waiting,
    messageKey: "taskStatusMessage_ready",
    nextStepKey: "taskStatusNext_ready",
    technicalDetailKey: "taskStatusDetail_ready",
  }, t);
}

export function presentTerminalTask(
  task: TimelineTerminalTask,
  t: Translate,
): ProductStatusPresentation {
  switch (task.status) {
    case "starting": return presentProductStatus("loading", t);
    case "running":
      return presentRunningTerminalTask(task.readiness, t);
    case "exited": {
      if (task.exitCode === undefined) {
        return localizeStatus({
          ...PRODUCT_STATUS_COPY.uncertain,
          messageKey: "terminalTaskMessage_exited",
          nextStepKey: "terminalTaskNext_exited",
          technicalDetailKey: "terminalTaskDetail_exited",
          values: { exitCode: "not recorded" },
        }, t);
      }
      return task.exitCode !== 0
        ? localizeStatus({
            ...PRODUCT_STATUS_COPY.failed,
            messageKey: "terminalTaskMessage_exitFailed",
            nextStepKey: "terminalTaskNext_exitFailed",
            technicalDetailKey: "terminalTaskDetail_exitFailed",
            values: { exitCode: task.exitCode },
          }, t)
        : localizeStatus({
            ...PRODUCT_STATUS_COPY.completed,
            messageKey: "terminalTaskMessage_exited",
            nextStepKey: "terminalTaskNext_exited",
            technicalDetailKey: "terminalTaskDetail_exited",
            values: { exitCode: task.exitCode },
          }, t);
    }
    case "failed": return presentProductStatus("failed", t);
    case "cancelled": return presentProductStatus("cancelled", t);
    case "interrupted": return presentProductStatus("interrupted", t);
  }
}

function presentRunningTerminalTask(
  readiness: TerminalReadinessStatus,
  t: Translate,
): ProductStatusPresentation {
  if (readiness === "waiting") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY.waiting,
      action: "stop",
      messageKey: "terminalTaskMessage_readinessWaiting",
      nextStepKey: "terminalTaskNext_readinessWaiting",
      technicalDetailKey: "terminalTaskDetail_readinessWaiting",
    }, t);
  }
  if (readiness === "failed" || readiness === "timed_out") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY.blocked,
      action: "stop",
      messageKey: "terminalTaskMessage_readinessBlocked",
      nextStepKey: "terminalTaskNext_readinessBlocked",
      technicalDetailKey: "terminalTaskDetail_readinessBlocked",
    }, t);
  }
  return presentProductStatus("running", t);
}

export function presentArtifactStatus(
  status: ToolArtifactAvailability,
  t: Translate,
): ProductStatusPresentation {
  if (status === "available") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY.completed,
      action: "review",
      messageKey: "artifactStatusMessage_available",
      nextStepKey: "artifactStatusNext_available",
      technicalDetailKey: "artifactStatusDetail_available",
    }, t);
  }
  return localizeStatus({
    ...PRODUCT_STATUS_COPY.blocked,
    messageKey: "artifactStatusMessage_unavailable",
    nextStepKey: "artifactStatusNext_unavailable",
    technicalDetailKey: "artifactStatusDetail_unavailable",
    values: { availability: status },
  }, t);
}

export function presentCleanupStatus(
  errors: readonly string[],
  t: Translate,
): ProductStatusPresentation | undefined {
  if (errors.length === 0) return undefined;
  return localizeStatus({
    ...PRODUCT_STATUS_COPY.blocked,
    messageKey: "cleanupStatusMessage_attention",
    nextStepKey: "cleanupStatusNext_attention",
    technicalDetailKey: "cleanupStatusDetail_attention",
    values: { count: errors.length },
  }, t);
}

export function presentDeliveryStatus(
  phase: "pending" | "accepted" | "delivery_uncertain",
  t: Translate,
): ProductStatusPresentation {
  if (phase === "delivery_uncertain") return presentProductStatus("uncertain", t);
  if (phase === "accepted") {
    return localizeStatus({
      ...PRODUCT_STATUS_COPY.waiting,
      messageKey: "deliveryStatusMessage_accepted",
      nextStepKey: "deliveryStatusNext_accepted",
      technicalDetailKey: "deliveryStatusDetail_accepted",
    }, t);
  }
  return localizeStatus({
    ...PRODUCT_STATUS_COPY.waiting,
    action: "review_approval",
    messageKey: "runStatusMessage_approval",
    nextStepKey: "runStatusNext_approval",
    technicalDetailKey: "runStatusDetail_approval",
  }, t);
}

function localizeStatus(copy: StatusCopy, t: Translate): ProductStatusPresentation {
  return {
    status: copy.status,
    action: copy.action,
    label: t(copy.labelKey),
    message: t(copy.messageKey, copy.values),
    nextStep: t(copy.nextStepKey, copy.values),
    technicalDetail: t(copy.technicalDetailKey, copy.values),
  };
}
