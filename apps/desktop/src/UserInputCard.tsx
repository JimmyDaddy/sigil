import { useEffect, useState } from "react";

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

type DraftValue = string | string[] | SingleSelectDraft;

interface UserInputCardProps {
  request: UserInputRequest;
  busy: boolean;
  failure: boolean;
  onDecision: (decision: UserInputDecision) => void;
  onResume?: () => void;
}

export function UserInputCard({ request, busy, failure, onDecision, onResume }: UserInputCardProps) {
  const { t } = useLocale();
  const [drafts, setDrafts] = useState<Record<string, DraftValue>>({});
  const [validation, setValidation] = useState<string>();

  useEffect(() => {
    setDrafts(Object.fromEntries(request.questions.map((question) => [
      question.id,
      question.field.kind === "multi_select"
        ? []
        : question.field.kind === "single_select"
          ? { kind: "option", optionId: "" }
          : "",
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
      if (typeof answer === "string") {
        setValidation(answer);
        return;
      }
      if (answer !== undefined) answers.push(answer);
    }
    onDecision({ kind: "submitted", answers });
  };
  const waitingForContinuation = request.status !== "requested";

  return (
    <section className="user-input-card sg-bounded-content" aria-labelledby="user-input-title">
      <header>
        <div>
          <span className="plan-card-eyebrow">{t("userInputRequired")}</span>
          <h3 id="user-input-title">{request.prompt}</h3>
        </div>
        <span className="user-input-status">{t(`userInputPurpose_${request.purpose}`)}</span>
      </header>

      {waitingForContinuation ? (
        <div className="user-input-recovery" role="status">
          <p>{request.status === "decision_accepted"
            ? t("userInputAcceptedRecovery")
            : t("userInputContinuationRunning")}</p>
          {request.answerReceipt?.answeredQuestionIds.length ? (
            <small>{t("userInputAnsweredFields", {
              fields: request.answerReceipt.answeredQuestionIds.join(", "),
            })}</small>
          ) : null}
        </div>
      ) : <div className="user-input-fields">
        {request.questions.map((question) => (
          <fieldset key={question.id} disabled={busy}>
            <legend>{question.header}{question.required ? " *" : ""}</legend>
            <p>{question.question}</p>
            {question.description === undefined ? null : <small>{question.description}</small>}
            <UserInputField
              question={question}
              value={drafts[question.id]}
              onChange={(value) => update(question.id, value)}
            />
          </fieldset>
        ))}
      </div>}

      {validation === undefined ? null : <div className="user-input-error" role="alert">{validation}</div>}
      {failure ? <div className="user-input-error" role="alert">{t("userInputDecisionFailed")}</div> : null}

      {waitingForContinuation ? (
        <div className="plan-card-actions">
          {request.status === "decision_accepted" && onResume !== undefined ? (
            <Button type="button" variant="primary" disabled={busy} onClick={onResume}>
              {busy ? t("userInputResuming") : t("userInputResume")}
            </Button>
          ) : null}
        </div>
      ) : <div className="plan-card-actions">
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

function UserInputField({
  question,
  value,
  onChange,
}: {
  question: UserInputQuestion;
  value: DraftValue | undefined;
  onChange: (value: DraftValue) => void;
}) {
  const field = question.field;
  switch (field.kind) {
    case "text":
      return field.multiline ? (
        <TextArea label={question.header} labelHidden value={typeof value === "string" ? value : ""} maxLength={field.maxChars} onChange={(event) => onChange(event.target.value)} />
      ) : (
        <TextField label={question.header} labelHidden type="text" value={typeof value === "string" ? value : ""} maxLength={field.maxChars} onChange={(event) => onChange(event.target.value)} />
      );
    case "number":
      return <TextField label={question.header} labelHidden type="number" step="any" value={typeof value === "string" ? value : ""} onChange={(event) => onChange(event.target.value)} />;
    case "integer":
      return <TextField label={question.header} labelHidden type="number" step="1" value={typeof value === "string" ? value : ""} onChange={(event) => onChange(event.target.value)} />;
    case "boolean":
      return (
        <Select
          label={question.header}
          labelHidden
          value={typeof value === "string" ? value : ""}
          onChange={(event) => onChange(event.target.value)}
        >
          <option value="">Select…</option>
          <option value="true">Yes</option>
          <option value="false">No</option>
        </Select>
      );
    case "single_select": {
      const selected = isSingleSelectDraft(value)
        ? value
        : { kind: "option" as const, optionId: "" };
      const selectedIndex = selected.kind === "option"
        ? field.options.findIndex((option) => option.id === selected.optionId)
        : -1;
      return (
        <div className="user-input-select">
          <Select
            label={question.header}
            labelHidden
            value={selected.kind === "other" ? "other" : selectedIndex < 0 ? "" : `option:${selectedIndex}`}
            onChange={(event) => {
              if (event.target.value === "other") {
                onChange({ kind: "other", value: "" });
                return;
              }
              const optionIndex = Number(event.target.value.slice("option:".length));
              const option = field.options[optionIndex];
              onChange({ kind: "option", optionId: option?.id ?? "" });
            }}
          >
            <option value="">Select…</option>
            {field.options.map((option, index) => <option key={option.id} value={`option:${index}`}>{option.label}</option>)}
            {field.allowOther ? <option value="other">Other…</option> : null}
          </Select>
          {selected.kind === "other" ? (
            <TextField
              label={`${question.header} Other`}
              labelHidden
              type="text"
              value={selected.value}
              onChange={(event) => onChange({ kind: "other", value: event.target.value })}
            />
          ) : null}
        </div>
      );
    }
    case "multi_select": {
      const selected = Array.isArray(value) ? value : [];
      return <div className="user-input-options">{field.options.map((option) => (
        <Checkbox
            key={option.id}
            label={option.label}
            checked={selected.includes(option.id)}
            disabled={!selected.includes(option.id) && selected.length >= field.maxSelected}
            onChange={(event) => onChange(event.target.checked
              ? [...selected, option.id]
              : selected.filter((id) => id !== option.id))}
          />
      ))}</div>;
    }
  }
}

function answerForQuestion(question: UserInputQuestion, draft: DraftValue | undefined): UserInputAnswer | string | undefined {
  const missing = `${question.header} requires an answer.`;
  switch (question.field.kind) {
    case "text": {
      const value = typeof draft === "string" ? draft : "";
      if (value.length === 0) return question.required ? missing : undefined;
      return { questionId: question.id, value: { kind: "text", value } };
    }
    case "number": {
      const value = typeof draft === "string" ? draft : "";
      if (value.length === 0) return question.required ? missing : undefined;
      if (!Number.isFinite(Number(value))) return `${question.header} must be a finite number.`;
      return { questionId: question.id, value: { kind: "number", value } };
    }
    case "integer": {
      const value = typeof draft === "string" ? draft : "";
      if (value.length === 0) return question.required ? missing : undefined;
      const parsed = Number(value);
      if (!Number.isSafeInteger(parsed)) return `${question.header} must be an integer.`;
      return { questionId: question.id, value: { kind: "integer", value: parsed } };
    }
    case "boolean": {
      const value = typeof draft === "string" ? draft : "";
      if (value.length === 0) return question.required ? missing : undefined;
      return { questionId: question.id, value: { kind: "boolean", value: value === "true" } };
    }
    case "single_select": {
      const selected = isSingleSelectDraft(draft)
        ? draft
        : { kind: "option" as const, optionId: "" };
      if (selected.kind === "other") {
        const other = selected.value;
        if (other.length === 0) return `${question.header} requires an Other value.`;
        return { questionId: question.id, value: { kind: "single_select", other } };
      }
      if (selected.optionId.length === 0) return question.required ? missing : undefined;
      return { questionId: question.id, value: { kind: "single_select", optionId: selected.optionId } };
    }
    case "multi_select": {
      const optionIds = Array.isArray(draft) ? draft : [];
      if (optionIds.length === 0) return question.required ? missing : undefined;
      return { questionId: question.id, value: { kind: "multi_select", optionIds } };
    }
  }
}

function isSingleSelectDraft(value: DraftValue | undefined): value is SingleSelectDraft {
  return typeof value === "object" && !Array.isArray(value) && value !== null
    && (value.kind === "option" || value.kind === "other");
}
