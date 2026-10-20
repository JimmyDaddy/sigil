import { useEffect, useRef, type RefObject } from "react";

import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import { useLocale } from "./i18n";
import { presentDeliveryStatus } from "./statusPresentation";
import type {
  ApprovalAction,
  SessionGrantUnavailableReasonCode,
  TimelineApproval,
} from "./types";
import { Button, Collapsible } from "./ui/primitives";

export function ApprovalDock({
  approval,
  phase,
  busy,
  composerRef,
  onDecision,
  onOpenSettings,
  onOpenSupport,
}: {
  approval: TimelineApproval;
  phase: "pending" | "accepted" | "delivery_uncertain";
  busy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onDecision: (decision: ApprovalAction) => void;
  onOpenSettings: () => void;
  onOpenSupport: () => void;
}) {
  const { locale, t } = useLocale();
  const dockRef = useRef<HTMLElement>(null);
  const reviewRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (phase !== "pending") {
      composerRef.current?.focus({ preventScroll: true });
      return;
    }
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const target = approval.previewBody?.trim() ? reviewRef.current : dockRef.current;
    target?.focus();
    return () => {
      if (document.contains(composerRef.current)) composerRef.current?.focus({ preventScroll: true });
      else if (document.contains(previousFocus ?? null)) previousFocus?.focus({ preventScroll: true });
    };
  }, [approval.approvalRequestId, composerRef, phase]);

  const expiresAt = new Date(approval.expiresAtMs);
  const expires = Number.isFinite(expiresAt.getTime())
    ? new Intl.DateTimeFormat(locale, {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }).format(expiresAt)
    : t("notDescribed");
  const expired = Date.now() >= approval.expiresAtMs;
  const fileReviewUnavailable = requiresFileReview(approval)
    && !approval.previewBody?.trim();
  const commandReviewUnavailable = requiresCommandReview(approval)
    && !approval.previewBody?.trim();
  const reviewUnavailable = fileReviewUnavailable || commandReviewUnavailable;
  const unsupportedShellAnalysis = approval.analysisStatus === "unsupported"
    || approval.analysisStatus === "invalid"
    || approval.analysisReasonCodes?.some((code) =>
      code === "unsupported_syntax" || code === "invalid_syntax"
    ) === true;
  const deliveryPresentation = presentDeliveryStatus(phase, t);
  if (phase !== "pending") {
    return (
      <section
        className={`approval-dock approval-dock-${phase}`}
        ref={dockRef}
        aria-live="polite"
        aria-labelledby="approval-title"
        data-approval-product-status={deliveryPresentation.status}
      >
        <header>
          <div>
            <p className="eyebrow">
              {phase === "accepted" ? t("approvalAcceptedTitle") : t("approvalUncertainTitle")}
            </p>
            <h3 id="approval-title">{approval.previewTitle ?? approval.safeSummaryTitle ?? approval.toolName}</h3>
          </div>
          <span className={`approval-status status-${deliveryPresentation.status}`}>
            {deliveryPresentation.label}
          </span>
        </header>
        <p>{deliveryPresentation.message}</p>
        <small><strong>{t("nextStepLabel")}:</strong> {deliveryPresentation.nextStep}</small>
        <details>
          <summary>{t("showDetails")}</summary>
          <p><strong>{t("technicalDetailsLabel")}:</strong> {deliveryPresentation.technicalDetail}</p>
        </details>
      </section>
    );
  }
  return (
    <section
      className="approval-dock"
      ref={dockRef}
      tabIndex={-1}
      aria-labelledby="approval-title"
      data-approval-product-status={deliveryPresentation.status}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          composerRef.current?.focus({ preventScroll: true });
        }
      }}
    >
      <header>
        <div><p className="eyebrow">{t("approvalRequiredTitle")}</p><h3 id="approval-title">{approval.previewTitle ?? approval.safeSummaryTitle ?? approval.toolName}</h3></div>
        <span className={`risk-badge risk-${approval.risk ?? "unknown"}`}>{approval.risk ?? t("notClassified")}</span>
      </header>
      <div ref={reviewRef} className="approval-review" role="region" aria-label={t("requestedAction")} tabIndex={0}>
      <p>{approval.previewSummary ?? approval.safeSummaryDetail ?? t("reviewExactToolAction")}</p>
      {approval.analysisStatus === "conservative" ? (
        <p className="approval-risk-notice" role="note">{t("approvalConservativeNotice")}</p>
      ) : null}
      {approval.toolInput && !approval.previewBody?.trim() ? (
        <div className="approval-command">
          <strong>{t("requestedAction")}</strong>
          <pre>{approval.toolInput}</pre>
        </div>
      ) : null}
      {approval.previewBody ? (
        isUnifiedDiff(approval.previewBody)
          ? <DiffViewer diff={approval.previewBody} />
          : <pre>{approval.previewBody}</pre>
      ) : null}
      <Collapsible label={t("technicalDetailsLabel")} className="approval-technical-details">
      <dl>
        <div><dt>{t("approvalTool")}</dt><dd>{approval.toolName}</dd></div>
        <div><dt>{t("approvalAction")}</dt><dd>{approval.operation ?? t("notDescribed")}</dd></div>
        {approval.effects?.length ? <div><dt>{t("approvalEffects")}</dt><dd>{approval.effects.join(", ")}</dd></div> : null}
        {approval.subjects?.length ? <div><dt>{t("approvalTargets")}</dt><dd>{approval.subjects.join(", ")}</dd></div> : null}
        {approval.containment?.length ? <div><dt>{t("approvalContainment")}</dt><dd>{approval.containment.join(" · ")}</dd></div> : null}
        {approval.analysisStatus ? <div><dt>{t("approvalAnalysis")}</dt><dd>{approval.analysisStatus}</dd></div> : null}
        <div><dt>{t("fileSnapshot")}</dt><dd>{approval.snapshotRequired ? t("required") : t("notRequired")}</dd></div>
        <div><dt>{t("decisionExpires")}</dt><dd>{expires}</dd></div>
      </dl>
      {approval.analysisReasons?.length || approval.decisionReasons?.length ? (
        <div className="approval-reasons">
          <strong>{t("approvalWhy")}</strong>
          <ul>
            {[...(approval.analysisReasons ?? []), ...(approval.decisionReasons ?? [])].map((reason, index) => (
              <li key={`${index}-${reason}`}>{reason}</li>
            ))}
          </ul>
        </div>
      ) : null}
      </Collapsible>
      {unsupportedShellAnalysis ? (
        <aside className="approval-remediation" role="note">
          <div>
            <strong>{t("unsupportedShellAnalysisTitle")}</strong>
            <p>{t("unsupportedShellAnalysisDetail")}</p>
          </div>
          <div className="approval-remediation-actions">
            <Button variant="quiet" type="button" onClick={onOpenSettings}>{t("openSettings")}</Button>
            <Button variant="quiet" type="button" onClick={onOpenSupport}>{t("openSupport")}</Button>
          </div>
        </aside>
      ) : null}
      {reviewUnavailable ? (
        <aside className="approval-remediation" role="alert">
          <div>
            <strong>{t(commandReviewUnavailable ? "approvalCommandReviewUnavailableTitle" : "approvalFileReviewUnavailableTitle")}</strong>
            <p>{t(commandReviewUnavailable ? "approvalCommandReviewUnavailableDetail" : "approvalFileReviewUnavailableDetail")}</p>
          </div>
          <Button variant="quiet" type="button" onClick={onOpenSupport}>{t("openSupport")}</Button>
        </aside>
      ) : <small>
        {approval.sessionGrantAvailable
          ? t("approveSessionDetail")
          : t(sessionGrantUnavailableMessageKey(approval.sessionGrantUnavailableReason?.code))}
      </small>}
      </div>
      {expired ? <div className="approval-expired" role="alert">{t("approvalExpired")}</div> : null}
      <div className="approval-actions">
        <Button variant="danger" type="button" disabled={busy || expired} onClick={() => onDecision("deny")}>{t("deny")}</Button>
        {approval.commandFamilyAllowPattern ? (
          <Button variant="quiet" type="button" disabled={busy || expired || reviewUnavailable} onClick={() => onDecision("approve_family")}>{t("approveFamily")}</Button>
        ) : null}
        {approval.sessionGrantAvailable ? (
          <Button variant="quiet" type="button" disabled={busy || expired || reviewUnavailable} onClick={() => onDecision("approve_session")}>{t("approveSession")}</Button>
        ) : null}
        <Button variant="primary" type="button" disabled={busy || expired || reviewUnavailable} onClick={() => onDecision("approve_once")}>{t("approveOnce")}</Button>
      </div>
    </section>
  );
}

function requiresFileReview(approval: TimelineApproval): boolean {
  if (approval.effects?.some((effect) => effect === "file_write" || effect === "file_delete")) return true;
  // These are typed tool operations, never inferred from prompts or tool names.
  return new Set([
    "create_file", "edit_file", "overwrite_file", "delete_file", "rename_path",
    "create_directory", "delete_directory", "recursive_delete", "apply_change_set",
  ]).has(approval.operation ?? "");
}

function requiresCommandReview(approval: TimelineApproval): boolean {
  return new Set([
    "execute_read_only_command", "execute_workspace_check_command", "execute_mutating_command",
    "execute_unknown_command", "execute_destructive_command",
  ]).has(approval.operation ?? "");
}

function sessionGrantUnavailableMessageKey(code?: SessionGrantUnavailableReasonCode) {
  switch (code) {
    case "analysis_incomplete": return "sessionGrantUnavailableAnalysisIncomplete" as const;
    case "semantic_scope_unavailable": return "sessionGrantUnavailableSemanticScope" as const;
    case "non_grantable_effect": return "sessionGrantUnavailableEffect" as const;
    case "containment_binding_unavailable": return "sessionGrantUnavailableContainment" as const;
    case "policy_decision_not_grantable": return "sessionGrantUnavailablePolicy" as const;
    case "no_reusable_approval_facet": return "sessionGrantUnavailableNoFacet" as const;
    case "network_scope_not_grantable": return "sessionGrantUnavailableNetwork" as const;
    case "confirmation_required": return "sessionGrantUnavailableConfirmation" as const;
    case "snapshot_required": return "sessionGrantUnavailableSnapshot" as const;
    case "subject_scope_unavailable": return "sessionGrantUnavailableSubject" as const;
    case "risk_not_grantable": return "sessionGrantUnavailableRisk" as const;
    case "external_mutation": return "sessionGrantUnavailableExternalMutation" as const;
    case "operation_not_grantable": return "sessionGrantUnavailableOperation" as const;
    default: return "sessionGrantUnavailableUnknown" as const;
  }
}
