import { useLocale } from "./i18n";
import type { TimelineTerminalTask } from "./types";
import { Button } from "./ui/primitives";

interface TerminalTaskCardProps {
  readonly task: TimelineTerminalTask;
  readonly stopping?: boolean;
  readonly onStop?: () => void;
}

export function TerminalTaskCard({ task, stopping = false, onStop }: TerminalTaskCardProps) {
  const { t } = useLocale();
  const active = task.status === "starting" || task.status === "running";
  const statusLabel = t(`terminalTaskStatus_${task.status}`);
  const readinessLabel = t(`terminalTaskReadiness_${task.readiness}`);

  return (
    <article
      className={`terminal-task-card terminal-task-${task.status}`}
      data-terminal-task-id={task.taskId}
      data-terminal-task-status={task.status}
      data-terminal-task-readiness={task.readiness}
      data-terminal-task-generation={task.generation}
      data-terminal-task-backend={task.executionBackend}
      data-terminal-task-sandbox={task.sandboxProfile}
    >
      <header>
        <span>
          <small>{t("backgroundTerminalTask")}</small>
          <strong>{task.taskId}</strong>
        </span>
        <span className={`terminal-task-status status-${task.status}`}>{statusLabel}</span>
      </header>
      <dl className="terminal-task-meta">
        <div>
          <dt>{t("terminalTaskGeneration")}</dt>
          <dd>{task.generation}</dd>
        </div>
        <div>
          <dt>{t("terminalTaskReadiness")}</dt>
          <dd>{readinessLabel}</dd>
        </div>
        {task.executionBackend === undefined ? null : (
          <div>
            <dt>{t("terminalTaskBackend")}</dt>
            <dd>{formatMachineLabel(task.executionBackend)}</dd>
          </div>
        )}
        {task.sandboxProfile === undefined ? null : (
          <div>
            <dt>{t("terminalTaskSandbox")}</dt>
            <dd>{formatMachineLabel(task.sandboxProfile)}</dd>
          </div>
        )}
        <div>
          <dt>{t("terminalTaskOutput")}</dt>
          <dd>{formatBytes(task.totalOutputBytes)}</dd>
        </div>
        {task.exitCode === undefined ? null : (
          <div>
            <dt>{t("terminalTaskExitCode")}</dt>
            <dd>{task.exitCode}</dd>
          </div>
        )}
      </dl>
      {task.failureReason === undefined && task.readinessFailureReason === undefined ? null : (
        <p className="terminal-task-failure" role="alert">
          {task.failureReason ?? task.readinessFailureReason}
        </p>
      )}
      {!active || onStop === undefined ? null : (
        <footer>
          <Button type="button" variant="danger" busy={stopping} onClick={onStop}>
            {stopping ? t("terminalTaskStopping") : t("terminalTaskStop")}
          </Button>
        </footer>
      )}
    </article>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toFixed(1)} KB`;
  return `${(bytes / 1_048_576).toFixed(1)} MB`;
}

function formatMachineLabel(value: string): string {
  return value.split("_").join(" ");
}
