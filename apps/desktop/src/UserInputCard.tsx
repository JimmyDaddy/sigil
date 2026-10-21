import { useEffect, useId, useState } from "react";

import { useLocale } from "./i18n";
import type {
  UserInputAnswer,
  UserInputDecision,
  UserInputQuestion,
  UserInputRequest,
} from "./types";
import { Button, Checkbox, Select, TextArea, TextField } from "./ui/primitives";

type SingleSelectDraft =
  | { kind: "option"; optionId: string }
  | { kind: "other"; value: string };
type MultiSelectDraft = { optionIds: string[]; other: string };
type DraftValue = string | SingleSelectDraft | MultiSelectDraft;
interface AnswerValidationIssue {
  kind: "required_answer" | "required_other";
  question: string;
}

interface UserInputCardProps {
  request: UserInputRequest;
  busy: boolean;
  failure: boolean;
  queuePosition?: number;
  queueLength?: number;
  onPrevious?: () => void;
  onNext?: () => void;
  onDecision: (decision: UserInputDecision) => void;
  onResume?: () => void;
}

export function UserInputCard({
  request,
  busy,
  failure,
  queuePosition,
  queueLength,
  onPrevious,
  onNext,
  onDecision,
  onResume,
}: UserInputCardProps) {
  const { t } = useLocale();
  const titleId = useId();
  const [drafts, setDrafts] = useState<Record<string, DraftValue>>({});
  const [validation, setValidation] = useState<AnswerValidationIssue>();

  useEffect(() => {
    setDrafts(Object.fromEntries(request.questions.map((question) => [
      question.id,
      question.options.length === 0
        ? ""
        : question.multiple
          ? { optionIds: [], other: "" }
          : { kind: "option", optionId: "" },
    ])));
    setValidation(undefined);
  }, [request.identity.requestId, request.identity.generation, request.requestHash]);

  const update = (questionId: string, value: DraftValue) => {
    setDrafts((current) => ({ ...current, [questionId]: value }));
    setValidation(undefined);
  };

  const submit = () => {
    const answers: UserInputAnswer[] = [];
    for (const question of request.questions) {
      const answer = answerForQuestion(question, drafts[question.id]);
      if (answer !== undefined && "kind" in answer) {
        setValidation(answer);
        return;
      }
      if (answer !== undefined) answers.push(answer);
    }
    onDecision({ kind: "submitted", answers });
  };
  const waitingForContinuation = request.status !== "requested";
  const singleQuestionMatchesPrompt = request.questions.length === 1
    && request.questions[0].question === request.prompt;

  return (
    <section
      className={`user-input-card sg-bounded-content${waitingForContinuation ? " user-input-card-answered" : ""}`}
      aria-labelledby={titleId}
    >
      {waitingForContinuation ? <h3 id={titleId} className="sr-only">{request.prompt}</h3> : <header>
        <div>
          <span className="plan-card-eyebrow">{t("userInputRequired")}</span>
          <h3 id={titleId}>{request.prompt}</h3>
        </div>
        <span className="user-input-status">{t(`userInputPurpose_${request.purpose}`)}</span>
      </header>}

      {(queueLength ?? 0) > 1 ? (
        <nav className="plan-card-actions" aria-label={t("userInputQueueLabel")}>
          <Button type="button" variant="secondary" disabled={busy || onPrevious === undefined} onClick={onPrevious}>
            {t("userInputPrevious")}
          </Button>
          <span>{t("userInputQueuePosition", { position: queuePosition ?? 1, total: queueLength ?? 1 })}</span>
          <Button type="button" variant="secondary" disabled={busy || onNext === undefined} onClick={onNext}>
            {t("userInputNext")}
          </Button>
        </nav>
      ) : null}

      {waitingForContinuation ? (
        <div className="user-input-summary">
          <p role="status">{request.status === "decision_accepted"
            ? t("userInputAcceptedRecovery")
            : t("userInputContinuationRunning")}</p>
          {request.status === "decision_accepted" && onResume !== undefined ? (
            <Button type="button" variant="primary" disabled={busy} onClick={onResume}>
              {busy ? t("userInputResuming") : t("userInputResume")}
            </Button>
          ) : null}
        </div>
      ) : <div className="user-input-fields">
        {request.questions.map((question) => (
          <fieldset key={question.id} disabled={busy}>
            <legend className={singleQuestionMatchesPrompt ? "sr-only" : undefined}>{question.question}</legend>
            {question.description === undefined ? null : <small>{question.description}</small>}
            <UserInputQuestionField
              question={question}
              value={drafts[question.id]}
              onChange={(value) => update(question.id, value)}
            />
          </fieldset>
        ))}
      </div>}

      {waitingForContinuation || validation === undefined ? null : (
        <div className="user-input-error" role="alert">
          {t(validation.kind === "required_answer" ? "userInputAnswerRequired" : "userInputOtherRequired", {
            question: validation.question,
          })}
        </div>
      )}
      {failure ? <div className="user-input-error" role="alert">{t(waitingForContinuation ? "userInputContinuationFailed" : "userInputDecisionFailed")}</div> : null}

      {waitingForContinuation ? null : <div className="plan-card-actions">
        {request.allowedActions.includes("decline") ? (
          <Button type="button" variant="secondary" disabled={busy} onClick={() => onDecision({ kind: "declined" })}>
            {t("userInputDecline")}
          </Button>
        ) : null}
        {request.allowedActions.includes("cancel_run") ? (
          <Button type="button" variant="secondary" disabled={busy} onClick={() => onDecision({ kind: "run_cancelled" })}>
            {t("userInputCancelRun")}
          </Button>
        ) : null}
        <Button type="button" variant="primary" disabled={busy || !request.allowedActions.includes("submit")} onClick={submit}>
          {busy ? t("userInputSubmitting") : t("userInputSubmit")}
        </Button>
      </div>}
    </section>
  );
}

function UserInputQuestionField({
  question,
  value,
  onChange,
}: {
  question: UserInputQuestion;
  value: DraftValue | undefined;
  onChange: (value: DraftValue) => void;
}) {
  const { t } = useLocale();
  if (question.options.length === 0) {
    return (
      <TextArea
        label={question.question}
        labelHidden
        value={typeof value === "string" ? value : ""}
        maxLength={4096}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }

  if (!question.multiple) {
    const selected = isSingleSelectDraft(value) ? value : { kind: "option" as const, optionId: "" };
    return (
      <div className="user-input-select">
        <Select
          label={question.question}
          labelHidden
          value={selected.kind === "other" ? "other" : selected.optionId}
          onChange={(event) => {
            if (event.target.value === "other") {
              onChange({ kind: "other", value: "" });
            } else {
              onChange({ kind: "option", optionId: event.target.value });
            }
          }}
        >
          <option value="">{t("userInputSelectPlaceholder")}</option>
          {question.options.map((option) => <option key={option.id} value={option.id}>{option.label}</option>)}
          <option value="other">{t("userInputOtherOption")}</option>
        </Select>
        {selected.kind === "other" ? (
          <TextField
            label={t("userInputOtherAnswerLabel", { question: question.question })}
            labelHidden
            type="text"
            value={selected.value}
            onChange={(event) => onChange({ kind: "other", value: event.target.value })}
          />
        ) : null}
      </div>
    );
  }

  const selected = isMultiSelectDraft(value) ? value : { optionIds: [], other: "" };
  return (
    <div className="user-input-options">
      {question.options.map((option) => (
        <Checkbox
          key={option.id}
          label={option.label}
          checked={selected.optionIds.includes(option.id)}
          onChange={(event) => onChange({
            optionIds: event.target.checked
              ? [...selected.optionIds, option.id]
              : selected.optionIds.filter((id) => id !== option.id),
            other: selected.other,
          })}
        />
      ))}
      <TextField
        label={t("userInputOtherAnswerLabel", { question: question.question })}
        labelHidden
        type="text"
        value={selected.other}
        onChange={(event) => onChange({ optionIds: selected.optionIds, other: event.target.value })}
      />
    </div>
  );
}

function answerForQuestion(
  question: UserInputQuestion,
  draft: DraftValue | undefined,
): UserInputAnswer | AnswerValidationIssue | undefined {
  const missing: AnswerValidationIssue = { kind: "required_answer", question: question.question };
  if (question.options.length === 0) {
    const value = typeof draft === "string" ? draft : "";
    return value.length === 0
      ? question.required ? missing : undefined
      : { questionId: question.id, value: { kind: "text", value } };
  }
  if (!question.multiple) {
    const selected = isSingleSelectDraft(draft) ? draft : { kind: "option" as const, optionId: "" };
    if (selected.kind === "other") {
      return selected.value.length === 0
        ? question.required ? { kind: "required_other", question: question.question } : undefined
        : { questionId: question.id, value: { kind: "single_select", other: selected.value } };
    }
    return selected.optionId.length === 0
      ? question.required ? missing : undefined
      : { questionId: question.id, value: { kind: "single_select", optionId: selected.optionId } };
  }
  const selected = isMultiSelectDraft(draft) ? draft : { optionIds: [], other: "" };
  if (selected.optionIds.length === 0 && selected.other.length === 0) {
    return question.required ? missing : undefined;
  }
  return {
    questionId: question.id,
    value: {
      kind: "multi_select",
      optionIds: selected.optionIds,
      ...(selected.other.length === 0 ? {} : { other: selected.other }),
    },
  };
}

function isSingleSelectDraft(value: DraftValue | undefined): value is SingleSelectDraft {
  return typeof value === "object" && value !== null && "kind" in value
    && (value.kind === "option" || value.kind === "other");
}

function isMultiSelectDraft(value: DraftValue | undefined): value is MultiSelectDraft {
  return typeof value === "object" && value !== null && "optionIds" in value;
}
