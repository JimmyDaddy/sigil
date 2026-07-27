import { useEffect, useMemo, useState } from "react";

import { useLocale } from "./i18n";
import type {
  IntentDropBinding,
  IntentDropExecution,
  IntentDropPreview,
  IntentStackState,
  IntentSummary,
  IntentVersionRef,
} from "./types";
import { Button } from "./ui/primitives";

interface IntentStackInspectorProps {
  state?: IntentStackState;
  preview?: IntentDropPreview;
  execution?: IntentDropExecution;
  loading: boolean;
  busy: boolean;
  error: boolean;
  runActive: boolean;
  onPreview: (intentRef: IntentVersionRef) => void;
  onConfirm: (request: IntentDropBinding) => void;
  onRefresh: () => void;
}

export function IntentStackInspector({
  state,
  preview,
  execution,
  loading,
  busy,
  error,
  runActive,
  onPreview,
  onConfirm,
  onRefresh,
}: IntentStackInspectorProps) {
  const { t } = useLocale();
  const intents = state?.status === "available" ? state.stack.intents : [];
  const [selectedKey, setSelectedKey] = useState<string>();
  useEffect(() => {
    if (intents.length === 0) {
      setSelectedKey(undefined);
      return;
    }
    setSelectedKey((current) => (
      current !== undefined && intents.some((intent) => intentKey(intent.intentRef) === current)
        ? current
        : intentKey(intents.find((intent) => intent.applicationState !== "dropped")?.intentRef
          ?? intents[0].intentRef)
    ));
  }, [intents]);
  const selected = useMemo(
    () => intents.find((intent) => intentKey(intent.intentRef) === selectedKey) ?? intents[0],
    [intents, selectedKey],
  );
  const selectedPreview = selected !== undefined
    && preview?.targetIntents.some((intentRef) => intentKey(intentRef) === intentKey(selected.intentRef))
    ? preview
    : undefined;

  if (loading && state === undefined) {
    return <p className="intent-stack-empty">{t("loadingIntentStack")}</p>;
  }
  if (error) {
    return (
      <section className="intent-stack-empty" role="alert">
        <strong>{t("intentStackUnavailable")}</strong>
        <p>{t("intentStackUnavailableDetail")}</p>
        <Button type="button" variant="quiet" disabled={busy} onClick={onRefresh}>
          {t("refreshIntentStack")}
        </Button>
      </section>
    );
  }
  if (state?.status === "history_unavailable") {
    return (
      <section className="intent-stack-empty">
        <strong>{t("intentHistoryUnavailable")}</strong>
        <p>{state.safeMessage}</p>
        <Button type="button" variant="quiet" disabled={busy} onClick={onRefresh}>
          {t("refreshIntentStack")}
        </Button>
      </section>
    );
  }
  if (state?.status !== "available" || selected === undefined) {
    return <p className="intent-stack-empty">{t("noIntentsRecorded")}</p>;
  }

  const canPreview =
    state.stack.authorityState === "active"
    && state.stack.conflicts.length === 0
    && selected.availableActions.includes("drop")
    && selected.applicationState !== "dropped";
  const previewBlocked = selectedPreview !== undefined && (
    !selectedPreview.targetIsLeaf || selectedPreview.conflicts.length > 0
  );

  return (
    <div className="intent-stack-inspector">
      <header className="intent-stack-header">
        <div>
          <span>{t("intentStackVersion", { version: state.stack.stackVersion })}</span>
          <strong>{t("intentCount", { count: intents.length })}</strong>
        </div>
        <span className={`intent-authority intent-authority-${state.stack.authorityState}`}>
          {formatMachineLabel(state.stack.authorityState)}
        </span>
      </header>

      <div className="intent-stack-body">
        <nav className="intent-stack-list" aria-label={t("intentList")}>
          {intents.map((intent) => (
            <Button
              key={intentKey(intent.intentRef)}
              type="button"
              variant="quiet"
              className="intent-stack-row"
              aria-selected={intentKey(intent.intentRef) === intentKey(selected.intentRef)}
              onClick={() => setSelectedKey(intentKey(intent.intentRef))}
            >
              <span>
                <strong>{intent.title}</strong>
                <small>{formatMachineLabel(intent.applicationState)}</small>
              </span>
              <span className="intent-stack-row-count">
                {intent.exclusiveArtifactCount + intent.sharedArtifactCount}
              </span>
            </Button>
          ))}
        </nav>

        <section className="intent-stack-detail" aria-labelledby="selected-intent-title">
          <header>
            <div>
              <span className="intent-stack-eyebrow">{formatMachineLabel(selected.definitionState)}</span>
              <h3 id="selected-intent-title">{selected.title}</h3>
            </div>
            <span className={`intent-state intent-state-${selected.applicationState}`}>
              {formatMachineLabel(selected.applicationState)}
            </span>
          </header>
          <p>{selected.statement}</p>
          <IntentMetrics intent={selected} />

          {selected.dependsOn.length > 0 ? (
            <section className="intent-stack-section">
              <h4>{t("intentDependencies")}</h4>
              <p>{selected.dependsOn.join(" · ")}</p>
            </section>
          ) : null}
          <section className="intent-stack-section">
            <h4>{t("intentSource")}</h4>
            <p>{intentSourceLabel(selected, t)}</p>
          </section>
          <section className="intent-stack-section">
            <h4>{t("acceptanceCriteria")}</h4>
            <ul>
              {selected.acceptanceCriteria.map((criterion) => (
                <li key={criterion.criterionId}>
                  <span aria-hidden="true">{criterion.required ? "●" : "○"}</span>
                  {criterion.statement}
                </li>
              ))}
            </ul>
          </section>
          <section className="intent-stack-section">
            <h4>{t("intentArtifacts")}</h4>
            {selected.artifacts.length === 0 ? (
              <p>{t("noIntentArtifacts")}</p>
            ) : (
              <ul className="intent-artifact-list">
                {selected.artifacts.map((artifact) => (
                  <li key={artifact.artifactId}>
                    <span>{artifact.normalizedRelativePath ?? formatMachineLabel(artifact.artifactKind)}</span>
                    <small>{formatMachineLabel(artifact.ownership)} · {formatMachineLabel(artifact.availability)}</small>
                  </li>
                ))}
              </ul>
            )}
          </section>

          {state.stack.conflicts.length === 0 ? null : (
            <div className="intent-drop-conflicts" role="alert">
              <strong>{t("intentStackConflicts")}</strong>
              <ul>
                {state.stack.conflicts.map((conflict) => (
                  <li key={`${conflict.code}:${conflict.safeReason}`}>{conflict.safeReason}</li>
                ))}
              </ul>
            </div>
          )}

          {selectedPreview === undefined ? (
            <div className="intent-stack-actions">
              <Button
                type="button"
                variant="primary"
                busy={busy}
                disabled={busy || runActive || !canPreview}
                onClick={() => onPreview(selected.intentRef)}
              >
                {t("previewIntentDrop")}
              </Button>
              {!canPreview ? <small>{intentActionUnavailableReason(state.stack.authorityState, selected, t)}</small> : null}
              {runActive ? <small>{t("waitForRunBeforeIntentDrop")}</small> : null}
            </div>
          ) : (
            <IntentDropReview
              intent={selected}
              preview={selectedPreview}
              execution={execution}
              busy={busy}
              runActive={runActive}
              blocked={previewBlocked}
              onConfirm={() => onConfirm({
                operationId: selectedPreview.operationId,
                stackVersion: selectedPreview.stackVersion,
                previewDigest: selectedPreview.previewDigest,
              })}
            />
          )}
        </section>
      </div>
    </div>
  );
}

function IntentMetrics({ intent }: { readonly intent: IntentSummary }) {
  const { t } = useLocale();
  return (
    <dl className="intent-stack-metrics">
      <div><dt>{t("exclusive")}</dt><dd>{intent.exclusiveArtifactCount}</dd></div>
      <div><dt>{t("shared")}</dt><dd>{intent.sharedArtifactCount}</dd></div>
      <div><dt>{t("drifted")}</dt><dd>{intent.driftedArtifactCount}</dd></div>
      <div><dt>{t("verified")}</dt><dd>{intent.systemVerifiedCriterionCount}</dd></div>
    </dl>
  );
}

function IntentDropReview({
  intent,
  preview,
  execution,
  busy,
  runActive,
  blocked,
  onConfirm,
}: {
  readonly intent: IntentSummary;
  readonly preview: IntentDropPreview;
  readonly execution?: IntentDropExecution;
  readonly busy: boolean;
  readonly runActive: boolean;
  readonly blocked: boolean;
  readonly onConfirm: () => void;
}) {
  const { t } = useLocale();
  return (
    <section className={`intent-drop-review${blocked ? " is-blocked" : ""}`}>
      <header>
        <div>
          <span className="intent-stack-eyebrow">{t("exactIntentDropPreview")}</span>
          <h4>{t("dropIntent", { title: intent.title })}</h4>
        </div>
        <span>{preview.fileEffects.length} {t("files")}</span>
      </header>
      <p>{blocked ? t("intentDropBlockedDetail") : t("intentDropExactDetail")}</p>
      {preview.fileEffects.length === 0 ? null : (
        <ul className="intent-drop-effects">
          {preview.fileEffects.map((effect) => (
            <li key={`${effect.action}:${effect.normalizedRelativePath}`}>
              <strong>{formatMachineLabel(effect.action)}</strong>
              <code>{effect.normalizedRelativePath}</code>
            </li>
          ))}
        </ul>
      )}
      {preview.conflicts.length === 0 ? null : (
        <div className="intent-drop-conflicts" role="alert">
          <strong>{t("intentDropConflicts")}</strong>
          <ul>{preview.conflicts.map((conflict) => <li key={`${conflict.code}:${conflict.safeReason}`}>{conflict.safeReason}</li>)}</ul>
        </div>
      )}
      <dl className="intent-drop-impact">
        <div><dt>{t("retainedIntents")}</dt><dd>{preview.retainedIntents.length}</dd></div>
        <div><dt>{t("verificationImpacts")}</dt><dd>{preview.verificationImpacts.length}</dd></div>
        <div><dt>{t("workspaceRevision")}</dt><dd>{preview.workspaceRevision}</dd></div>
      </dl>
      <details className="intent-drop-binding">
        <summary>{t("exactReviewBindings")}</summary>
        <code>{preview.previewDigest}</code>
      </details>
      {execution !== undefined ? (
        <p className={`intent-drop-result intent-drop-result-${execution.resolution}`} role="status">
          {t("intentDropResult", { status: formatMachineLabel(execution.resolution) })}
        </p>
      ) : null}
      <div className="intent-stack-actions">
        <Button
          type="button"
          variant="danger"
          busy={busy}
          disabled={busy || runActive || blocked || execution?.resolution === "committed"}
          onClick={onConfirm}
        >
          {t("confirmIntentDrop", { title: intent.title })}
        </Button>
        <small>{runActive ? t("waitForRunBeforeIntentDrop") : t("intentDropCannotUndoExternalEffects")}</small>
      </div>
    </section>
  );
}

function intentKey(intentRef: IntentVersionRef): string {
  return `${intentRef.intentId}:${intentRef.version}`;
}

function intentActionUnavailableReason(
  authorityState: "active" | "read_only_provenance" | "out_of_scope",
  intent: IntentSummary,
  t: ReturnType<typeof useLocale>["t"],
): string {
  if (authorityState !== "active") return t("intentAuthorityReadOnly");
  if (intent.applicationState === "dropped") return t("intentAlreadyDropped");
  return t("intentDropUnavailable");
}

function intentSourceLabel(intent: IntentSummary, t: ReturnType<typeof useLocale>["t"]): string {
  switch (intent.source.kind) {
    case "user_turn":
      return t("intentSourceUserTurn", { source: intent.source.sourceTurnId });
    case "accepted_suggestion":
      return t("intentSourceSuggestion", { source: intent.source.sourceTurnId });
    case "trusted_spec":
      return t("intentSourceTrustedSpec", { source: intent.source.safeSourceLabel });
  }
}

function formatMachineLabel(value: string): string {
  return value.split("_").join(" ");
}
