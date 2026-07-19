import { useEffect, useRef, type RefObject } from "react";

import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import type { TimelineApproval } from "./types";

export function ApprovalDock({
  approval,
  busy,
  composerRef,
  onDecision,
}: {
  approval: TimelineApproval;
  busy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onDecision: (approve: boolean) => void;
}) {
  const dockRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    dockRef.current?.focus();
    return () => {
      if (document.contains(composerRef.current)) composerRef.current?.focus();
      else if (document.contains(previousFocus ?? null)) previousFocus?.focus();
    };
  }, [approval.approvalRequestId, composerRef]);

  const expires = new Intl.DateTimeFormat(undefined, {
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
          composerRef.current?.focus();
        }
      }}
    >
      <header>
        <div><p className="eyebrow">Approval required</p><h3 id="approval-title">{approval.previewTitle ?? approval.toolName}</h3></div>
        <span className={`risk-badge risk-${approval.risk ?? "unknown"}`}>{approval.risk ?? "not classified"}</span>
      </header>
      <p>{approval.previewSummary ?? "Review this exact tool action before the run can continue."}</p>
      {approval.previewBody ? (
        isUnifiedDiff(approval.previewBody)
          ? <DiffViewer diff={approval.previewBody} />
          : <pre>{approval.previewBody}</pre>
      ) : null}
      <dl>
        <div><dt>Tool</dt><dd>{approval.toolName}</dd></div>
        <div><dt>Action</dt><dd>{approval.operation ?? "not described"}</dd></div>
        <div><dt>File snapshot</dt><dd>{approval.snapshotRequired ? "required" : "not required"}</dd></div>
        <div><dt>Decision expires</dt><dd>{expires}</dd></div>
      </dl>
      <small>“Approve once” applies only to this exact request. It cannot undo file, shell, or remote side effects that already happened.</small>
      {expired ? <div className="approval-expired" role="alert">This decision expired. Reopen the conversation to refresh the run state.</div> : null}
      <div className="approval-actions">
        <button className="quiet-button danger-button" type="button" disabled={busy || expired} onClick={() => onDecision(false)}>Deny</button>
        <button className="primary-button" type="button" disabled={busy || expired} onClick={() => onDecision(true)}>Approve once</button>
      </div>
    </section>
  );
}
