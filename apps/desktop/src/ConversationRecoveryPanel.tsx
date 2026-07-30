import { useState } from "react";

import { useLocale } from "./i18n";
import type {
  CheckpointRestoreReview,
  CheckpointView,
  ConversationForkReceipt,
  ConversationRecoveryView,
} from "./types";
import { Icon } from "./ui/icons";
import { Button } from "./ui/primitives";

export function ConversationRecoveryPanel({
  recovery,
  preview,
  busy,
  error,
  onRefresh,
  onCompact,
  onPreview,
  onRestore,
  onFork,
}: {
  readonly recovery?: ConversationRecoveryView;
  readonly preview?: CheckpointRestoreReview;
  readonly busy: boolean;
  readonly error: boolean;
  readonly onRefresh: () => void;
  readonly onCompact: () => Promise<boolean>;
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
          <Button
            type="button"
            busy={busy}
            onClick={() => void onCompact()}
          >
            {t(busy ? "compactingContext" : "compactNow")}
          </Button>
        </header>
        {busy ? (
          <p className="conversation-recovery-empty-copy" aria-live="polite">
            {t("compactingContext")}
          </p>
        ) : null}
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
