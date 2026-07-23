import { useEffect, useRef, type RefObject } from "react";

import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import { useLocale } from "./i18n";
import type { ApprovalAction, TimelineApproval } from "./types";
import { Button } from "./ui/primitives";

export function ApprovalDock({
  approval,
  busy,
  composerRef,
  onDecision,
}: {
  approval: TimelineApproval;
  busy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onDecision: (decision: ApprovalAction) => void;
}) {
  const { locale, t } = useLocale();
  const dockRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    dockRef.current?.focus();
    return () => {
      if (document.contains(composerRef.current)) composerRef.current?.focus({ preventScroll: true });
      else if (document.contains(previousFocus ?? null)) previousFocus?.focus({ preventScroll: true });
    };
  }, [approval.approvalRequestId, composerRef]);

  const expires = new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(approval.expiresAtMs);
  const expired = Date.now() >= approval.expiresAtMs;
  return (
    <section
      className="approval-dock"
      ref={dockRef}
      tabIndex={-1}
      aria-labelledby="approval-title"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          composerRef.current?.focus({ preventScroll: true });
        }
      }}
    >
      <header>
        <div><p className="eyebrow">{t("approvalRequiredTitle")}</p><h3 id="approval-title">{approval.previewTitle ?? approval.toolName}</h3></div>
        <span className={`risk-badge risk-${approval.risk ?? "unknown"}`}>{approval.risk ?? t("notClassified")}</span>
      </header>
      <p>{approval.previewSummary ?? t("reviewExactToolAction")}</p>
      {approval.toolInput ? (
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
      <dl>
        <div><dt>{t("approvalTool")}</dt><dd>{approval.toolName}</dd></div>
        <div><dt>{t("approvalAction")}</dt><dd>{approval.operation ?? t("notDescribed")}</dd></div>
        <div><dt>{t("fileSnapshot")}</dt><dd>{approval.snapshotRequired ? t("required") : t("notRequired")}</dd></div>
        <div><dt>{t("decisionExpires")}</dt><dd>{expires}</dd></div>
      </dl>
      <small>{approval.sessionGrantAvailable ? t("approveSessionDetail") : t("approveOnceDetail")}</small>
      {expired ? <div className="approval-expired" role="alert">{t("approvalExpired")}</div> : null}
      <div className="approval-actions">
        <Button variant="danger" type="button" disabled={busy || expired} onClick={() => onDecision("deny")}>{t("deny")}</Button>
        {approval.sessionGrantAvailable ? (
          <Button variant="quiet" type="button" disabled={busy || expired} onClick={() => onDecision("approve_session")}>{t("approveSession")}</Button>
        ) : null}
        <Button variant="primary" type="button" disabled={busy || expired} onClick={() => onDecision("approve_once")}>{t("approveOnce")}</Button>
      </div>
    </section>
  );
}
