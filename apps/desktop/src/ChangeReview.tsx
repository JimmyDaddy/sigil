import { useState } from "react";

import type { ChangeReviewDraft, CheckpointReview, ReviewAnnotation, ReviewDiff, ReviewDiffSide } from "./features/conversation/reviewTypes";
import { useLocale } from "./i18n";
import { Button, TextArea } from "./ui/primitives";

export function ChangeReview({ review, draft, onDraftChange, onRefresh, onSend }: {
  readonly review: CheckpointReview;
  readonly draft: ChangeReviewDraft;
  readonly onDraftChange: (change: (current: ChangeReviewDraft) => ChangeReviewDraft) => void;
  readonly onRefresh: () => void;
  readonly onSend: (annotations: ReviewAnnotation[]) => Promise<boolean>;
}) {
  const { t } = useLocale();
  const [sending, setSending] = useState(false);
  const [failed, setFailed] = useState(false);
  const selected = review.diffs.find((diff) => diff.diffDigest === draft.selection?.diffDigest);
  const selection = draft.selection;
  const selectLine = (diff: ReviewDiff, side: ReviewDiffSide, line: number, extend: boolean) => {
    const previous = draft.selection;
    const same = previous?.diffDigest === diff.diffDigest && previous.side === side;
    const start = extend && same ? previous.startLine : line;
    onDraftChange((current) => ({ ...current, selection: {
      diffDigest: diff.diffDigest, side, startLine: Math.min(start, line), endLine: Math.max(start, line),
    } }));
  };
  const validRange = selected !== undefined && selection !== undefined
    && selection.endLine - selection.startLine < 200
    && new Set(selected.lines.map((line) => selection.side === "old" ? line.oldLine : line.newLine)
      .filter((line): line is number => line !== undefined && line >= selection.startLine && line <= selection.endLine)).size
      === selection.endLine - selection.startLine + 1;
  const add = () => {
    if (selected === undefined || selection === undefined || !validRange || draft.comment.trim() === "") return;
    const annotation: ReviewAnnotation = {
      checkpointId: review.checkpointId, checkpointDigest: review.checkpointDigest,
      sourceCallId: selected.sourceCallId, diffDigest: selected.diffDigest, path: selected.path,
      side: selection.side, startLine: selection.startLine, endLine: selection.endLine, comment: draft.comment,
    };
    onDraftChange((current) => ({ ...current, annotations: [...current.annotations, { ...annotation, draftId: current.nextId }], nextId: current.nextId + 1, comment: "" }));
  };
  const send = async () => {
    if (sending || draft.annotations.length === 0) return;
    const submitted = draft.annotations;
    const ids = new Set(submitted.map((annotation) => annotation.draftId));
    setSending(true);
    setFailed(false);
    try {
      const accepted = await onSend(submitted.map(({ draftId: _draftId, ...annotation }) => annotation));
      if (accepted) onDraftChange((current) => ({ ...current, annotations: current.annotations.filter((annotation) => !ids.has(annotation.draftId)) }));
      else setFailed(true);
    } catch {
      setFailed(true);
    } finally {
      setSending(false);
    }
  };
  return <section className="change-review">
    <p>{t("changeReviewDetail")}</p>
    <Button type="button" variant="quiet" onClick={onRefresh}>{t("refreshReview")}</Button>
    {review.diffs.length === 0 ? <p>{t("noRecordedDiff")}</p> : null}
    {review.diffs.map((diff) => <article className="checkpoint-diff" key={`${diff.sourceCallId}:${diff.diffDigest}`}>
      <header><strong>{diff.path}</strong><span>{t(diff.fileState === "current" ? "reviewFileCurrent" : diff.fileState === "changed" ? "reviewFileChanged" : "reviewFileUnknown")}</span></header>
      {diff.fileState !== "current" ? <p role="note">{t("reviewOutdatedDetail")}</p> : null}
      <pre className="change-review-lines" aria-label={t("unifiedDiff")}>
        {diff.lines.map((line, index) => <span className="change-review-line" key={index}>
          {(["old", "new"] as const).map((side) => {
            const number = side === "old" ? line.oldLine : line.newLine;
            const active = number !== undefined && selection?.diffDigest === diff.diffDigest && selection.side === side && number >= selection.startLine && number <= selection.endLine;
            return number === undefined ? <span key={side} /> : <Button variant="quiet" type="button" key={side} aria-pressed={active}
              aria-label={t("reviewSelectLine", { path: diff.path, side: t(side === "old" ? "reviewOldSide" : "reviewNewSide"), line: number })}
              onClick={(event) => selectLine(diff, side, number, event.shiftKey)}>{number}</Button>;
          })}
          <code>{line.text || " "}</code>
        </span>)}
      </pre>
      {diff.truncated ? <small>{t("diffTruncated")}</small> : null}
    </article>)}
    {review.truncated ? <p>{t("diffTruncated")}</p> : null}
    <p aria-live="polite">{selected !== undefined && selection !== undefined ? t("reviewSelectedRange", {
      path: selected.path, side: t(selection.side === "old" ? "reviewOldSide" : "reviewNewSide"), start: selection.startLine, end: selection.endLine,
    }) : t("reviewSelectHint")}</p>
    <TextArea label={t("reviewComment")} value={draft.comment} maxLength={4096}
      onChange={(event) => onDraftChange((current) => ({ ...current, comment: event.target.value }))} />
    <Button type="button" disabled={!validRange || draft.comment.trim() === "" || draft.annotations.length >= 16} onClick={add}>{t("addReviewComment")}</Button>
    <ol>{draft.annotations.map((annotation) => <li key={annotation.draftId}>
      <strong>{annotation.path}:{annotation.startLine}–{annotation.endLine} ({t(annotation.side === "old" ? "reviewOldSide" : "reviewNewSide")})</strong>
      <p>{annotation.comment}</p>
      <Button type="button" variant="quiet" onClick={() => onDraftChange((current) => ({ ...current, annotations: current.annotations.filter((item) => item.draftId !== annotation.draftId) }))}>{t("removeReviewComment")}</Button>
    </li>)}</ol>
    {failed ? <p role="alert">{t("reviewSendFailed")}</p> : null}
    <Button type="button" busy={sending} disabled={draft.annotations.length === 0} onClick={() => void send()}>{t(sending ? "sendingReview" : "sendReview")}</Button>
  </section>;
}
