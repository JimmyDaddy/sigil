import { useLocale } from "./i18n";
import type { ConversationPlanReview, PlanDecisionAction } from "./types";
import { Button } from "./ui/primitives";

interface PlanCardProps {
  review: ConversationPlanReview;
  disabled: boolean;
  busy: boolean;
  failure: boolean;
  onDecision: (action: PlanDecisionAction) => void;
}

export function PlanCard({
  review,
  disabled,
  busy,
  failure,
  onDecision,
}: PlanCardProps) {
  const { t } = useLocale();
  const actionDisabled = (action: PlanDecisionAction) =>
    disabled
    || busy
    || review.status !== "draft_ready"
    || (review.stale && (action === "run" || action === "save"));
  const primaryAction: PlanDecisionAction | undefined = review.allowedActions.includes("run")
    ? "run"
    : review.allowedActions[0];

  return (
    <section className="plan-card sg-bounded-content" aria-labelledby="plan-card-title">
      <header>
        <div>
          <span className="plan-card-eyebrow">{t("planReview")}</span>
          <h3 id="plan-card-title">
            {review.summary ?? t(`planReviewStatus_${review.status}`)}
          </h3>
        </div>
        <span className={`plan-status plan-status-${review.status.replace(/_/g, "-")}`}>
          {t(`planReviewStatus_${review.status}`)}
        </span>
      </header>

      {review.planHash === undefined ? null : (
        <div className="plan-card-meta">
          <code>{review.planId}</code>
          <code>{review.planHash.slice(0, 12)}…</code>
          {review.stepCount === undefined ? null : (
            <span>{t("planReviewSteps", { count: review.stepCount })}</span>
          )}
          {review.targetPathCount === undefined ? null : (
            <span>{t("planReviewTargetPaths", { count: review.targetPathCount })}</span>
          )}
          {review.suggestedCheckCount === undefined ? null : (
            <span>{t("planReviewSuggestedChecks", { count: review.suggestedCheckCount })}</span>
          )}
          <span>{t(`planReviewSource_${review.source}`)}</span>
        </div>
      )}

      {review.risk === undefined ? null : (
        <div className="plan-card-risk">
          <strong>{t("planRisk")}</strong>
          <span>{review.risk}</span>
        </div>
      )}

      {review.stale ? (
        <div className="plan-card-notice" role="status">
          {t("planReviewStale")}
        </div>
      ) : null}

      {failure ? (
        <div className="plan-card-notice plan-card-notice-error" role="alert">
          {t("planDecisionFailed")}
        </div>
      ) : null}

      {review.allowedActions.length === 0 ? null : (
        <div className="plan-card-actions">
          {review.allowedActions.map((action) => (
            <Button
              key={action}
              type="button"
              variant={action === "run" ? "primary" : "secondary"}
              disabled={actionDisabled(action)}
              onClick={() => onDecision(action)}
            >
              {busy && action === primaryAction
                ? t("planDecisionInProgress")
                : t(`planDecision_${action}`)}
            </Button>
          ))}
        </div>
      )}
    </section>
  );
}
