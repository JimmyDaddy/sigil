import { useState, type RefObject } from "react";

import { useLocale } from "./i18n";
import type { TaskProductProjection } from "./features/conversation/taskProjection";
import { Button, TextArea, Tooltip } from "./ui/primitives";

interface TaskControlPanelProps {
  task: TaskProductProjection;
  runActive: boolean;
  disabled: boolean;
  busy: boolean;
  reviewReady: boolean;
  reviewBusy: boolean;
  pauseDisabled: boolean;
  pauseBusy: boolean;
  reviewButtonRef?: RefObject<HTMLButtonElement | null>;
  onPause: () => void;
  onContinue: (guidance?: string) => void;
  onReviewIntegration: () => void;
}

export function TaskControlPanel({
  task,
  runActive,
  disabled,
  busy,
  reviewReady,
  reviewBusy,
  pauseDisabled,
  pauseBusy,
  reviewButtonRef,
  onPause,
  onContinue,
  onReviewIntegration,
}: TaskControlPanelProps) {
  const { t } = useLocale();
  const [guidanceOpen, setGuidanceOpen] = useState(false);
  const [guidance, setGuidance] = useState("");
  const childTotal = task.activeChildren + task.completedChildren + task.failedChildren;
  const actionDisabled = disabled || busy || runActive;
  const canContinue = task.canContinue && !reviewReady && !runActive;
  const canPause = runActive
    && task.execution !== undefined
    && ["started", "running"].includes(task.status.toLowerCase());

  return (
    <section className="task-control-panel sg-bounded-content" aria-labelledby="task-control-title">
      <header>
        <div>
          <span className="task-control-eyebrow">{t("task")}</span>
          <h3 id="task-control-title">{task.objective ?? t("taskInProgress")}</h3>
        </div>
        <div className="task-control-header-actions">
          <span className={`task-status task-status-${statusClass(task.status)}`}>
            {formatMachineLabel(task.status)}
          </span>
          {canPause ? (
            <Tooltip label={t("pauseTaskDetail")}>
              <Button
                type="button"
                variant="quiet"
                disabled={pauseDisabled || pauseBusy}
                onClick={onPause}
              >
                {pauseBusy ? t("pausingTask") : t("pauseTask")}
              </Button>
            </Tooltip>
          ) : null}
        </div>
      </header>

      <div className="task-control-meta">
        <code>{task.taskId}</code>
        {task.phase === undefined ? null : <span>{t("taskPhase", { phase: formatMachineLabel(task.phase) })}</span>}
        {task.planVersion === undefined ? null : <span>{t("taskPlanVersion", { version: task.planVersion })}</span>}
        {childTotal === 0 ? null : (
          <span>{t("taskChildProgress", {
            completed: task.completedChildren,
            total: childTotal,
            failed: task.failedChildren,
          })}</span>
        )}
      </div>

      {task.checklist.length === 0 ? null : (
        <ol className="task-step-list task-checklist">
          {task.checklist.map((item) => (
            <li key={item.itemId}>
              <span className={`task-step-dot task-step-${statusClass(item.status)}`} aria-hidden="true" />
              <span>
                <strong>{item.text}</strong>
                <small>{formatMachineLabel(item.status)}</small>
              </span>
            </li>
          ))}
        </ol>
      )}

      {task.checklist.length !== 0 || task.steps.length === 0 ? null : (
        <ol className="task-step-list">
          {task.steps.map((step) => (
            <li key={step.stepId}>
              <span className={`task-step-dot task-step-${statusClass(step.status ?? "pending")}`} aria-hidden="true" />
              <span>
                <strong>{step.title}</strong>
                <small>{step.status === undefined ? step.role : `${step.role} · ${formatMachineLabel(step.status)}`}</small>
              </span>
            </li>
          ))}
        </ol>
      )}

      {task.lanes.length === 0 ? null : (
        <div className="task-lane-summary">
          <strong>{t("taskIntegrationLanes", { count: task.lanes.length })}</strong>
          <span>{task.lanes.map((lane) => `${lane.laneId} · ${formatMachineLabel(lane.status ?? "pending")}`).join(" · ")}</span>
        </div>
      )}

      {reviewReady ? (
        <div className="task-control-actions">
          <span>{t("taskIntegrationReady")}</span>
          <Button ref={reviewButtonRef} type="button" variant="primary" disabled={actionDisabled} onClick={onReviewIntegration}>
            {t("reviewTaskIntegration")}
          </Button>
        </div>
      ) : reviewBusy && task.phase === "integration" ? (
        <div className="task-control-actions">
          <span>{t("preparingTaskIntegration")}</span>
        </div>
      ) : canContinue ? (
        <>
          <div className="task-control-actions">
            {task.execution?.kind === "direct" ? null : (
              <Button type="button" variant="quiet" disabled={actionDisabled} onClick={() => setGuidanceOpen((open) => !open)}>
                {guidanceOpen ? t("hideTaskGuidance") : t("addTaskGuidance")}
              </Button>
            )}
            <Button type="button" variant="primary" disabled={actionDisabled} onClick={() => onContinue()}>
              {busy ? t("continuingTask") : t("continueTask")}
            </Button>
          </div>
          {guidanceOpen && task.execution?.kind !== "direct" ? (
            <div className="task-guidance">
              <TextArea
                id={`task-guidance-${task.taskId}`}
                label={t("taskGuidance")}
                value={guidance}
                maxLength={16_384}
                rows={3}
                placeholder={t("taskGuidancePlaceholder")}
                disabled={actionDisabled}
                onChange={(event) => setGuidance(event.currentTarget.value)}
              />
              <Button
                type="button"
                variant="primary"
                disabled={actionDisabled || guidance.trim() === ""}
                onClick={() => onContinue(guidance.trim())}
              >
                {busy ? t("continuingTask") : t("continueWithGuidance")}
              </Button>
            </div>
          ) : null}
        </>
      ) : null}
    </section>
  );
}

function formatMachineLabel(value: string): string {
  return value.replace(/_/g, " ");
}

function statusClass(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, "-");
}
