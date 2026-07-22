import { useState } from "react";

import { writeClipboard } from "./clipboard";
import { useLocale } from "./i18n";
import type { VerificationSummary } from "./types";
import { Icon } from "./ui/icons";
import { Button, Collapsible, IconButton, Tooltip } from "./ui/primitives";

export function VerificationInspector({
  verification,
  busy,
  runActive,
  onRerun,
}: {
  verification?: VerificationSummary;
  busy: boolean;
  runActive: boolean;
  onRerun: () => void;
}) {
  const { t } = useLocale();
  if (verification === undefined) {
    return (
      <div className="inspector-empty">
        <strong>{t("noVerificationEvidence")}</strong>
        <p>{t("noVerificationEvidenceDetail")}</p>
      </div>
    );
  }
  return (
    <section className="verification-card" aria-labelledby="verification-title">
      <header>
        <div><p className="eyebrow">{t("currentCheck")}</p><h3 id="verification-title">{verification.recommendedCheckSpecId ?? t("currentEvidence")}</h3></div>
        <span className={`verification-badge verification-${verification.verdict}`}>{verification.status}</span>
      </header>
      {verification.recommendationReason ? <p>{verification.recommendationReason}</p> : null}
      {verification.evidence.failureSummary ? (
        <div className="verification-failure" role="status">
          <strong>{t("failureLocation")}</strong>
          <p>{verification.evidence.failureSummary}</p>
        </div>
      ) : null}
      <div className="verification-actions">
        {verification.action?.kind === "rerun" ? (
          <Button variant="primary" type="button" busy={busy} disabled={runActive} onClick={onRerun}>
            {busy ? t("runningCheck") : verification.recommendationKind === "retry" ? t("retryCheck") : verification.recommendationKind === "rerun_non_writing" ? t("rerunNonWritingCheck") : t("runRecommendedCheck")}
          </Button>
        ) : verification.action?.kind === "review_approval" ? (
          <small>{t("separateTrustReview")}</small>
        ) : (
          <small>{t("noVerificationAction")}</small>
        )}
      </div>
      <Collapsible className="evidence-details" label={t("evidenceDetails")}>
        <EvidenceRow label={t("scope")} value={`${verification.scopeKind} · ${verification.scopeId}`} />
        <EvidenceRow label={t("receipt")} value={verification.evidence.receiptId} />
        <EvidenceRow label={t("snapshot")} value={verification.evidence.workspaceSnapshotId} />
        <EvidenceRow label={t("changeset")} value={verification.evidence.changesetId} />
        <EvidenceRow label={t("command")} value={verification.evidence.commandEventId} />
        <EvidenceRow label={t("output")} value={verification.evidence.outputArtifactId} />
      </Collapsible>
    </section>
  );
}

function EvidenceRow({ label, value }: { label: string; value?: string }) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  return (
    <div className="evidence-row">
      <span>{label}</span><code>{value ?? t("notLinked")}</code>
      {value !== undefined ? (
        <Tooltip label={copied ? t("copied") : t("copyItem", { item: label.toLowerCase() })}>
          <IconButton
            className="evidence-copy"
            type="button"
            onClick={() => void writeClipboard(value).then(setCopied)}
            aria-label={t("copyItem", { item: label.toLowerCase() })}
            icon={<Icon name={copied ? "check" : "copy"} />}
          />
        </Tooltip>
      ) : null}
    </div>
  );
}
