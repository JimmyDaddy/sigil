import { useState } from "react";

import { useLocale } from "./i18n";
import type {
  CheckpointRestoreReview,
  CheckpointView,
  CompactionDetails,
  CompactionReview,
  ConversationForkReceipt,
  ConversationRecoveryView,
} from "./types";
import { Icon } from "./ui/icons";
import { Button } from "./ui/primitives";

function CompactionToolArtifacts({ details }: { readonly details: CompactionDetails }) {
  const { t } = useLocale();
  if (details.toolArtifacts.length === 0) return null;
  return (
    <div className="compaction-tool-artifacts">
      <strong>{t("compactionRecoverableToolArtifacts")}</strong>
      <ul>
        {details.toolArtifacts.map((artifact) => (
          <li key={`${artifact.sourceEventId}:${artifact.toolCallId}`}>
            <details>
              <summary>{artifact.toolName} · {artifact.status}</summary>
              <dl>
                <div><dt>{t("compactionArtifactSource")}</dt><dd>{artifact.sourceEventId}</dd></div>
                <div><dt>{t("compactionArtifactHash")}</dt><dd>{artifact.contentSha256}</dd></div>
                <div><dt>{t("compactionArtifactSize")}</dt><dd>{artifact.originalContentBytes} B / {artifact.originalContentTokenUpperBound} tokens</dd></div>
              </dl>
              <p><strong>{t("compactionArtifactHead")}</strong></p>
              <pre>{artifact.headExcerpt}</pre>
              <p><strong>{t("compactionArtifactTail")}</strong></p>
              <pre>{artifact.tailExcerpt}</pre>
              <small>{artifact.recoveryInstruction}</small>
            </details>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function ConversationRecoveryPanel({
  recovery,
  compaction,
  preview,
  busy,
  error,
  onRefresh,
  onPreviewCompaction,
  onPrepareCompaction,
  onApplyStandaloneToolOutputShrink,
  onApplyCompaction,
  onPreview,
  onRestore,
  onFork,
}: {
  readonly recovery?: ConversationRecoveryView;
  readonly compaction?: CompactionReview;
  readonly preview?: CheckpointRestoreReview;
  readonly busy: boolean;
  readonly error: boolean;
  readonly onRefresh: () => void;
  readonly onPreviewCompaction: () => void;
  readonly onPrepareCompaction: () => Promise<void>;
  readonly onApplyStandaloneToolOutputShrink: () => Promise<void>;
  readonly onApplyCompaction: () => Promise<void>;
  readonly onPreview: (checkpoint: CheckpointView) => Promise<void>;
  readonly onRestore: (checkpoint: CheckpointView) => Promise<void>;
  readonly onFork: (sourceTurnDigest: string) => Promise<ConversationForkReceipt | undefined>;
}) {
  const { t } = useLocale();
  const [selectedCheckpointId, setSelectedCheckpointId] = useState<string>();

  const selected = recovery?.checkpoints.find(
    (checkpoint) => checkpoint.checkpointId === selectedCheckpointId,
  );
  const selectedPreview = selected !== undefined
    && preview?.checkpointId === selected.checkpointId
    && preview.checkpointDigest === selected.checkpointDigest
    ? preview
    : undefined;

  return (
    <div className="conversation-recovery-panel sg-bounded-content">
      <section className="conversation-recovery-section compaction-review">
        <header>
          <div>
            <h3>{t("compactContext")}</h3>
            <p>{t("compactContextDetail")}</p>
          </div>
          <Button type="button" variant="quiet" busy={busy} onClick={onPreviewCompaction}>
            {t(compaction === undefined ? "previewCompaction" : "refreshPreview")}
          </Button>
        </header>
        {compaction === undefined ? (
          <p className="conversation-recovery-empty-copy">{t("compactPreviewFirst")}</p>
        ) : compaction.admission.kind === "prepared" ? (
          <div className="compaction-admission is-ready" aria-live="polite">
            <dl>
              <div><dt>{t("historyFolded")}</dt><dd>{compaction.foldedEventCount}</dd></div>
              <div><dt>{t("historyRetained")}</dt><dd>{compaction.retainedEventCount}</dd></div>
              <div><dt>{t("compactionToolArtifacts")}</dt><dd>{compaction.details?.toolArtifactCount ?? 0}</dd></div>
            </dl>
            {compaction.details === undefined ? null : (
              <div className="compaction-continuity-preview">
                <strong>{t("compactionActiveObjective")}</strong>
                <p>{compaction.details.activeObjective}</p>
                <CompactionToolArtifacts details={compaction.details} />
              </div>
            )}
            <p>{t("compactionLocalPreviewDetail")}</p>
            <div className="conversation-recovery-actions">
              <Button
                type="button"
                busy={busy}
                disabled={compaction.previewId === undefined}
                onClick={() => void onPrepareCompaction()}
              >
                {t("generateCompactionSummary")}
              </Button>
              <Button
                type="button"
                variant="quiet"
                busy={busy}
                disabled={
                  compaction.previewId === undefined
                  || !compaction.admission.standaloneToolOutputShrinkAvailable
                }
                onClick={() => void onApplyStandaloneToolOutputShrink()}
              >
                {t("cleanToolOutputsOnly")}
              </Button>
            </div>
          </div>
        ) : compaction.admission.kind === "ready" ? (
          <div className="compaction-admission is-ready" aria-live="polite">
            <dl>
              <div><dt>{t("historyFolded")}</dt><dd>{compaction.foldedEventCount}</dd></div>
              <div><dt>{t("historyRetained")}</dt><dd>{compaction.retainedEventCount}</dd></div>
              <div><dt>{t("estimatedTokensSaved")}</dt><dd>{compaction.admission.economics.savingsTokens}</dd></div>
              <div>
                <dt>{t("compactionSummaryUsage")}</dt>
                <dd>
                  {compaction.admission.economics.summaryCacheReadTokens} /{" "}
                  {compaction.admission.economics.summaryUncachedInputTokens} /{" "}
                  {compaction.admission.economics.summaryOutputTokens}
                </dd>
              </div>
              {compaction.admission.economics.summaryCostNanoUsd === undefined ? null : (
                <div>
                  <dt>{t("compactionSummaryCost")}</dt>
                  <dd>{compaction.admission.economics.summaryCostNanoUsd} nUSD</dd>
                </div>
              )}
              {compaction.policy === undefined ? null : (
                <>
                  <div><dt>{t("compactionStrategy")}</dt><dd>{compaction.policy.strategy}</dd></div>
                  <div><dt>{t("compactionPhase")}</dt><dd>{compaction.policy.phase}</dd></div>
                  <div>
                    <dt>{t("nativeCarrier")}</dt>
                    <dd>{t(compaction.policy.nativeCarrierAvailable ? "available" : "unavailable")}</dd>
                  </div>
                </>
              )}
              {compaction.details === undefined ? null : (
                <>
                  <div><dt>{t("compactionFoldedTurns")}</dt><dd>{compaction.details.foldedCompleteTurnCount}</dd></div>
                  <div><dt>{t("compactionFoldedTokens")}</dt><dd>{compaction.details.foldedTokenUpperBound}</dd></div>
                  <div><dt>{t("compactionRetainedTurns")}</dt><dd>{compaction.details.retainedCompleteTurnCount}</dd></div>
                  <div><dt>{t("compactionRetainedTokens")}</dt><dd>{compaction.details.retainedTokenUpperBound}</dd></div>
                  <div><dt>{t("compactionToolArtifacts")}</dt><dd>{compaction.details.toolArtifactCount}</dd></div>
                  <div><dt>{t("compactionPendingWork")}</dt><dd>{compaction.details.pendingWorkCount}</dd></div>
                  <div><dt>{t("compactionUnresolvedQuestions")}</dt><dd>{compaction.details.unresolvedQuestionCount}</dd></div>
                  <div><dt>{t("compactionRecoverableAttachments")}</dt><dd>{compaction.details.recoverableAttachmentCount}</dd></div>
                  <div><dt>{t("compactionProtectedControls")}</dt><dd>{compaction.details.protectedControlEventCount}</dd></div>
                  <div><dt>{t("compactionProtectedTools")}</dt><dd>{compaction.details.protectedActiveToolOrApprovalCount}</dd></div>
                  {compaction.details.currentCacheReadTokens === undefined ? null : (
                    <div><dt>{t("compactionCacheRead")}</dt><dd>{compaction.details.currentCacheReadTokens}</dd></div>
                  )}
                  {compaction.details.breakEvenTurns === undefined ? null : (
                    <div><dt>{t("compactionBreakEven")}</dt><dd>{compaction.details.breakEvenTurns}</dd></div>
                  )}
                </>
              )}
            </dl>
            {compaction.details === undefined ? null : (
              <div className="compaction-continuity-preview">
                <strong>{t("compactionActiveObjective")}</strong>
                <p>{compaction.details.activeObjective}</p>
                {compaction.details.activeConstraints.length === 0 ? null : (
                  <>
                    <strong>{t("compactionActiveConstraints")}</strong>
                    <ul>
                      {compaction.details.activeConstraints.map((constraint) => (
                        <li key={`${constraint.sourceEventId}:${constraint.sourceFieldPath}`}>
                          <span>{constraint.text}</span>
                          <small>{t("compactionConstraintSource", {
                            event: constraint.sourceEventId,
                            field: constraint.sourceFieldPath,
                          })}</small>
                        </li>
                      ))}
                    </ul>
                  </>
                )}
                <CompactionToolArtifacts details={compaction.details} />
              </div>
            )}
            {compaction.policy?.legacyMigrationFields.length ? (
              <p>{t("compactionLegacyMigration", {
                fields: compaction.policy.legacyMigrationFields.join(", "),
              })}</p>
            ) : null}
            <p>{t("compactionExplicitApply")}</p>
            <Button
              type="button"
              busy={busy}
              disabled={compaction.previewId === undefined}
              onClick={() => void onApplyCompaction()}
            >
              {t("applyCompaction")}
            </Button>
          </div>
        ) : compaction.admission.kind === "no_foldable_history" ? (
          <div className="compaction-admission" aria-live="polite">
            <strong>{t("nothingToCompact")}</strong>
            <p>{t("nothingToCompactDetail", {
              durable: compaction.admission.durableMessageCount,
              tail: compaction.admission.configuredTailMessageCount,
            })}</p>
          </div>
        ) : (
          <div className="compaction-admission is-unavailable" role="alert">
            <strong>{t("compactionUnavailable")}</strong>
            <p>{compaction.admission.reason}</p>
          </div>
        )}
      </section>

      <section className="conversation-recovery-safety" role="note">
        <Icon name="shield" />
        <div>
          <strong>{t("controlledRestoreOnly")}</strong>
          <p>{t("controlledRestoreOnlyDetail")}</p>
        </div>
      </section>

      {recovery === undefined ? (
        <div className="conversation-recovery-empty">
          <Icon name="history" />
          <p>{error ? t("conversationRecoveryUnavailable") : t("loadingConversationRecovery")}</p>
          {error ? <Button type="button" onClick={onRefresh}>{t("retry")}</Button> : null}
        </div>
      ) : <>

      <section className="conversation-recovery-section">
        <header>
          <div>
            <h3>{t("checkpoints")}</h3>
            <p>{t("checkpointRecoveryDetail")}</p>
          </div>
          <span>{recovery.checkpoints.length}</span>
        </header>
        {recovery.checkpoints.length === 0 ? (
          <p className="conversation-recovery-empty-copy">{t("noCheckpoints")}</p>
        ) : (
          <ol className="conversation-recovery-list">
            {recovery.checkpoints.map((checkpoint) => (
              <li key={checkpoint.checkpointId} className={checkpoint.checkpointId === selectedCheckpointId ? "is-selected" : ""}>
                <div>
                  <strong>{t("turnNumber", { count: checkpoint.turnIndex })}</strong>
                  <p>{checkpoint.prompt ?? t("checkpointWithoutPrompt")}</p>
                  <small>{t("controlledFilesCount", { count: checkpoint.files.length })}</small>
                </div>
                <Button
                  type="button"
                  variant="quiet"
                  busy={busy && checkpoint.checkpointId === selectedCheckpointId}
                  onClick={() => {
                    setSelectedCheckpointId(checkpoint.checkpointId);
                    void onPreview(checkpoint);
                  }}
                >
                  {t("previewRestore")}
                </Button>
              </li>
            ))}
          </ol>
        )}
      </section>

      {selected !== undefined && selectedPreview !== undefined ? (
        <section className="checkpoint-restore-review" aria-live="polite">
          <header>
            <div>
              <h3>{t("reverseDiffPreview")}</h3>
              <p>{selectedPreview.ready ? t("restoreReady") : t("restoreHasConflicts")}</p>
            </div>
            <span className={selectedPreview.ready ? "is-ready" : "is-conflict"}>
              {selectedPreview.ready ? t("ready") : t("blocked")}
            </span>
          </header>
          {selectedPreview.files.some((file) => file.conflictReason !== undefined) ? (
            <ul className="checkpoint-conflicts">
              {selectedPreview.files.filter((file) => file.conflictReason !== undefined).map((file) => (
                <li key={file.path}><strong>{file.path}</strong><span>{file.conflictReason}</span></li>
              ))}
            </ul>
          ) : null}
          {selectedPreview.reverseDiffs.length === 0 ? (
            <p className="conversation-recovery-empty-copy">{t("noReverseDiff")}</p>
          ) : selectedPreview.reverseDiffs.map((diff) => (
            <article className="checkpoint-diff" key={diff.path}>
              <header><strong>{diff.path}</strong><small>{t("linesCount", { count: diff.originalLineCount })}</small></header>
              <pre><code>{diff.diff}</code></pre>
              {diff.truncated ? <small>{t("diffTruncated")}</small> : null}
            </article>
          ))}
          <div className="conversation-recovery-actions">
            <Button
              type="button"
              variant="danger"
              busy={busy}
              disabled={!selectedPreview.ready}
              onClick={() => void onRestore(selected)}
            >
              {t("restoreControlledFiles")}
            </Button>
          </div>
        </section>
      ) : null}

      <section className="conversation-recovery-section">
        <header>
          <div>
            <h3>{t("conversationForks")}</h3>
            <p>{t("conversationForksDetail")}</p>
          </div>
          <span>{recovery.forkPoints.length}</span>
        </header>
        {recovery.forkPoints.length === 0 ? (
          <p className="conversation-recovery-empty-copy">{t("noForkPoints")}</p>
        ) : (
          <ol className="conversation-recovery-list fork-list">
            {recovery.forkPoints.slice().reverse().map((point) => (
              <li key={point.sourceTurnDigest}>
                <div>
                  <strong>{t("forkAfterTurn", { count: point.sourceTurnIndex })}</strong>
                  <p>{t("forkKeepsOriginal")}</p>
                </div>
                <Button type="button" variant="quiet" busy={busy} onClick={() => void onFork(point.sourceTurnDigest)}>
                  {t("forkConversation")}
                </Button>
              </li>
            ))}
          </ol>
        )}
      </section>
      </>}
    </div>
  );
}
