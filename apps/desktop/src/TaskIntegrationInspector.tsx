import { useLocale } from "./i18n";
import type {
  TaskIntegrationAcceptance,
  TaskIntegrationReview,
} from "./types";
import { Button } from "./ui/primitives";

interface TaskIntegrationInspectorProps {
  review: TaskIntegrationReview;
  acceptance?: TaskIntegrationAcceptance;
  busy: boolean;
  runActive: boolean;
  onAccept: () => void;
  onRefresh: () => void;
}

export function TaskIntegrationInspector({
  review,
  acceptance,
  busy,
  runActive,
  onAccept,
  onRefresh,
}: TaskIntegrationInspectorProps) {
  const { t } = useLocale();
  const cleanupErrors = [
    acceptance?.promotionCleanupError,
    acceptance?.parentCleanupError,
  ].filter((value): value is string => value !== undefined);
  const continuationReady = acceptance?.canContinue === true
    && acceptance.promotionStatus === "promoted"
    && acceptance.parentVerdict === "passed";

  return (
    <div className="task-integration-inspector">
      <section className="task-integration-summary">
        <header>
          <div>
            <span className="task-control-eyebrow">{t("taskIntegrationReview")}</span>
            <h3>{review.request.taskId}</h3>
          </div>
          <span className={`task-status task-status-${acceptance?.promotionStatus ?? "prepared"}`}>
            {acceptance?.promotionStatus ?? t("readyForReview")}
          </span>
        </header>
        <p>{t("taskIntegrationReviewDetail")}</p>
        <dl>
          <div><dt>{t("target")}</dt><dd>{formatMachineLabel(review.targetKind)}</dd></div>
          <div><dt>{t("taskIntegrationLanesLabel")}</dt><dd>{review.lanes.length}</dd></div>
          <div><dt>{t("childEvidence")}</dt><dd>{review.childVerificationReceiptCount}</dd></div>
          <div><dt>{t("laneEvidence")}</dt><dd>{review.laneVerificationReceiptCount}</dd></div>
          <div><dt>{t("invalidatedEvidence")}</dt><dd>{review.verificationInvalidationCount}</dd></div>
          <div><dt>{t("parentVerification")}</dt><dd>{acceptance?.parentVerdict ?? (review.parentVerificationPending ? t("pending") : t("unavailable"))}</dd></div>
        </dl>
      </section>

      {review.conflictReasons.length === 0 ? null : (
        <section className="task-integration-conflicts" aria-label={t("taskIntegrationConflicts")}>
          <strong>{t("taskIntegrationConflicts")}</strong>
          <ul>{review.conflictReasons.map((reason) => <li key={reason}>{formatMachineLabel(reason)}</li>)}</ul>
        </section>
      )}

      <section className="task-integration-lanes">
        <h4>{t("taskIntegrationLanesLabel")}</h4>
        {review.lanes.map((lane) => (
          <article key={lane.laneId}>
            <strong>{lane.laneId}</strong>
            <span>{formatMachineLabel(lane.candidateKind)}</span>
            <small>{t("taskLaneEvidence", {
              proposals: lane.proposalCount,
              receipts: lane.verificationReceiptCount,
            })}</small>
          </article>
        ))}
      </section>

      <section className="task-integration-diff">
        <header>
          <h4>{t("aggregateDiff")}</h4>
          <code>{review.aggregateDiffDigest}</code>
        </header>
        <pre aria-label={t("aggregateDiff")}>{review.aggregateDiff}</pre>
      </section>

      <details className="task-integration-bindings">
        <summary>{t("exactReviewBindings")}</summary>
        <dl>
          <dt>{t("request")}</dt><dd>{review.request.requestId}</dd>
          <dt>{t("plan")}</dt><dd>{review.request.planId} · v{review.request.planVersion}</dd>
          <dt>{t("preview")}</dt><dd>{review.previewDigest}</dd>
          <dt>{t("policy")}</dt><dd>{review.policyDigest}</dd>
        </dl>
      </details>

      {cleanupErrors.length === 0 ? null : (
        <div className="task-integration-cleanup" role="status">
          <strong>{t("cleanupNeedsAttention")}</strong>
          {cleanupErrors.map((error) => <span key={error}>{error}</span>)}
        </div>
      )}

      <div className="task-integration-actions">
        {acceptance !== undefined && !continuationReady ? (
          <Button type="button" variant="quiet" disabled={busy || runActive} onClick={onRefresh}>
            {t("refreshTaskIntegration")}
          </Button>
        ) : null}
        <Button
          type="button"
          variant="primary"
          disabled={busy || runActive || acceptance !== undefined}
          onClick={onAccept}
        >
          {busy ? t("acceptingTaskIntegration") : t("acceptAndContinueTask")}
        </Button>
      </div>
    </div>
  );
}

function formatMachineLabel(value: string): string {
  return value.replace(/_/g, " ");
}
