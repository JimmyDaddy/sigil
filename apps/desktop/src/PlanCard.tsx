import { useLocale } from "./i18n";
import type { ConversationPlanReview, PlanDecisionAction, PlanReviewDetail, PlanSuggestedCheckDetail } from "./types";
import { Button } from "./ui/primitives";

interface PlanCardProps {
  review: ConversationPlanReview;
  disabled: boolean;
  busy: boolean;
  failure: boolean;
  detail?: PlanReviewDetail;
  detailOpen: boolean;
  detailBusy: boolean;
  detailFailure: boolean;
  onOpenDetail: () => void;
  onCloseDetail: () => void;
  onDecision: (action: PlanDecisionAction) => void;
}

export function PlanCard({
  review,
  disabled,
  busy,
  failure,
  detail,
  detailOpen,
  detailBusy,
  detailFailure,
  onOpenDetail,
  onCloseDetail,
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
    <section className={`plan-card sg-bounded-content${detailOpen ? " plan-card-open" : ""}`} aria-labelledby="plan-card-title">
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

      {review.revision === undefined ? null : (
        <div className="plan-card-notice" role="status">
          {t(`planRevisionStatus_${review.revision.status}`)}
          {review.revision.attemptOrdinal === undefined
            ? null
            : ` · #${review.revision.attemptOrdinal}`}
        </div>
      )}

      {review.planHash === undefined ? null : (
        <div className="plan-card-review-toggle">
          <Button
            type="button"
            variant="quiet"
            data-plan-detail-toggle
            disabled={detailBusy}
            aria-expanded={detailOpen}
            onClick={detailOpen ? onCloseDetail : onOpenDetail}
          >
            {detailBusy
              ? t("planDetailLoading")
              : detailOpen
                ? t("planDetailClose")
                : t("planDetailOpen")}
          </Button>
        </div>
      )}

      {detailFailure ? (
        <div className="plan-card-notice plan-card-notice-error" role="alert">
          {t("planDetailFailed")}
        </div>
      ) : null}

      {detailOpen && detail !== undefined ? (
        <div className="plan-workbench" data-testid="plan-workbench">
          <section>
            <h4>{t("planDetailSummary")}</h4>
            <p>{detail.summary}</p>
          </section>
          <section>
            <h4>{t("planDetailSteps")}</h4>
            <ol className="plan-workbench-steps">
              {detail.steps.map((step) => (
                <li key={step.stepId}>
                  <strong>{step.title}</strong>
                  {step.detail === undefined ? null : <p>{step.detail}</p>}
                  <div className="plan-workbench-contract">
                    {step.role === undefined ? null : <code>{step.role}</code>}
                    {step.mode === undefined ? null : <code>{step.mode}</code>}
                    {step.isolation === undefined ? null : <code>{step.isolation}</code>}
                  </div>
                  {step.dependsOn.length === 0 ? null : (
                    <p><b>{t("planDetailDependsOn")}</b> {step.dependsOn.join(", ")}</p>
                  )}
                  {step.targetPaths.length === 0 ? null : (
                    <p><b>{t("planDetailPaths")}</b> {step.targetPaths.join(", ")}</p>
                  )}
                  {step.suggestedChecks.length === 0 ? null : (
                    <ul>
                      {step.suggestedChecks.map((check) => (
                        <li key={check.checkSpecId}><PlanCheck check={check} /></li>
                      ))}
                    </ul>
                  )}
                  {step.risk === undefined ? null : <p><b>{t("planRisk")}</b> {step.risk}</p>}
                  {step.notes.map((note) => <p key={note}>{note}</p>)}
                </li>
              ))}
            </ol>
          </section>
          {detail.targetPaths.length === 0 ? null : (
            <section>
              <h4>{t("planDetailScope")}</h4>
              <ul>{detail.targetPaths.map((path) => <li key={path}><code>{path}</code></li>)}</ul>
            </section>
          )}
          {detail.suggestedChecks.length === 0 ? null : (
            <section>
              <h4>{t("planDetailVerification")}</h4>
              <ul>{detail.suggestedChecks.map((check) => (
                <li key={check.checkSpecId}><PlanCheck check={check} /></li>
              ))}</ul>
            </section>
          )}
          {detail.notes.length === 0 ? null : (
            <section><h4>{t("planDetailNotes")}</h4><ul>{detail.notes.map((note) => <li key={note}>{note}</li>)}</ul></section>
          )}
        </div>
      ) : null}

      {failure ? (
        <div className="plan-card-notice plan-card-notice-error" role="alert">
          {t("planDecisionFailed")}
        </div>
      ) : null}

      {!detailOpen || detail === undefined || review.allowedActions.length === 0 ? null : (
        <div className="plan-card-actions">
          {review.allowedActions.map((action) => (
            <Button
              key={action}
              type="button"
              variant={action === "run" ? "primary" : "secondary"}
              data-plan-action={action}
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

function PlanCheck({ check }: { check: PlanSuggestedCheckDetail }) {
  return (
    <>
      <code>{[check.command, ...check.args].join(" ")}</code>
      <span className="plan-workbench-contract">
        <code>{check.effect}</code>
        {check.cwd === undefined ? null : <code>cwd={check.cwd}</code>}
        {check.sourceLine === undefined ? null : <code>source={check.sourceLine}</code>}
      </span>
    </>
  );
}
