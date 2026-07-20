import { useState } from "react";

import { writeClipboard } from "./clipboard";
import type { VerificationSummary } from "./types";
import { Button, Collapsible } from "./ui/primitives";

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
  if (verification === undefined) {
    return (
      <div className="inspector-empty">
        <strong>No verification evidence yet</strong>
        <p>Run a task or select a recorded check when evidence becomes available.</p>
      </div>
    );
  }
  return (
    <section className="verification-card" aria-labelledby="verification-title">
      <header>
        <div><p className="eyebrow">Current check</p><h3 id="verification-title">{verification.recommendedCheckSpecId ?? "Current evidence"}</h3></div>
        <span className={`verification-badge verification-${verification.verdict}`}>{verification.status}</span>
      </header>
      {verification.recommendationReason ? <p>{verification.recommendationReason}</p> : null}
      {verification.evidence.failureSummary ? (
        <div className="verification-failure" role="status">
          <strong>Failure location</strong>
          <p>{verification.evidence.failureSummary}</p>
        </div>
      ) : null}
      <div className="verification-actions">
        {verification.action?.kind === "rerun" ? (
          <Button variant="primary" type="button" busy={busy} disabled={runActive} onClick={onRerun}>
            {busy ? "Running check…" : verification.recommendationKind === "retry" ? "Retry check" : verification.recommendationKind === "rerun_non_writing" ? "Rerun non-writing check" : "Run recommended check"}
          </Button>
        ) : verification.action?.kind === "review_approval" ? (
          <small>This check needs a separate trust review.</small>
        ) : (
          <small>No verification action is currently required.</small>
        )}
      </div>
      <Collapsible className="evidence-details" label="Evidence details">
        <EvidenceRow label="Scope" value={`${verification.scopeKind} · ${verification.scopeId}`} />
        <EvidenceRow label="Receipt" value={verification.evidence.receiptId} />
        <EvidenceRow label="Snapshot" value={verification.evidence.workspaceSnapshotId} />
        <EvidenceRow label="Changeset" value={verification.evidence.changesetId} />
        <EvidenceRow label="Command" value={verification.evidence.commandEventId} />
        <EvidenceRow label="Output" value={verification.evidence.outputArtifactId} />
      </Collapsible>
    </section>
  );
}

function EvidenceRow({ label, value }: { label: string; value?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="evidence-row">
      <span>{label}</span><code>{value ?? "not linked"}</code>
      {value !== undefined ? (
        <Button className="evidence-copy" variant="quiet" type="button" onClick={() => void writeClipboard(value).then(setCopied)} aria-label={`Copy ${label.toLowerCase()}`}>
          {copied ? "Copied" : "Copy"}
        </Button>
      ) : null}
    </div>
  );
}
