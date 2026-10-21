import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider, useLocale, type Locale } from "./i18n";
import type { UserInputRequest } from "./types";
import { UserInputCard } from "./UserInputCard";

function acceptedRequest(): UserInputRequest {
  return {
    identity: {
      sessionScopeId: "session-1",
      rootLogicalRunId: "root-1",
      sourceThreadId: "main",
      requestId: "request-1",
      generation: 1,
      sourceBindingHash: `sha256:${"a".repeat(64)}`,
    },
    requestHash: `sha256:${"b".repeat(64)}`,
    source: { kind: "agent" },
    purpose: "clarification",
    prompt: "Which workspace should Sigil inspect?",
    questions: [{
      id: "workspace",
      question: "Which workspace should Sigil inspect?",
      required: true,
      options: [],
      multiple: false,
    }],
    allowedActions: ["submit", "decline", "cancel_run"],
    requestedAtUnixMs: 1,
    status: "decision_accepted",
    answerReceipt: {
      commandId: "command-1",
      decision: "submitted",
      answerHash: `sha256:${"c".repeat(64)}`,
      answeredQuestionIds: ["workspace"],
    },
  };
}

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(cleanup);

function requestedRequest(): UserInputRequest {
  return {
    ...acceptedRequest(),
    status: "requested",
    answerReceipt: undefined,
    prompt: "请选择本次需求草稿的验收方式（必填）。",
    questions: [{
      id: "acceptance",
      question: "请选择本次需求草稿的验收方式（必填）。",
      description: "Only the selected answer is submitted.",
      required: true,
      options: [{ id: "read_only", label: "只读检查" }],
      multiple: false,
    }],
  };
}

const localized = {
  en: {
    submit: "Submit and continue",
    placeholder: "Select…",
    other: "Other…",
    otherLabel: " — Other",
    required: (question: string) => `Answer this question: ${question}`,
    otherRequired: (question: string) => `Enter an Other answer: ${question}`,
  },
  "zh-CN": {
    submit: "提交并继续",
    placeholder: "请选择…",
    other: "其他…",
    otherLabel: " — 其他",
    required: (question: string) => `请回答：${question}`,
    otherRequired: (question: string) => `请填写其他选项：${question}`,
  },
};

function renderRequested(request: UserInputRequest, locale: Locale = "en") {
  window.localStorage.setItem("sigil.desktop.locale.v1", locale);
  const onDecision = vi.fn();
  const rendered = render(
    <LocaleProvider>
      <UserInputCard request={request} busy={false} failure={false} onDecision={onDecision} />
    </LocaleProvider>,
  );
  return { ...rendered, onDecision };
}

describe("UserInputCard question presentation and local validation", () => {
  it.each(["en", "zh-CN"] as const)("shows one visible identical single question with an accessible required select in %s", async (locale) => {
    const user = userEvent.setup();
    const request = requestedRequest();
    const { container, onDecision } = renderRequested(request, locale);
    const copy = localized[locale];

    expect(screen.getByRole("heading", { name: request.prompt })).toBeTruthy();
    const group = screen.getByRole("group", { name: request.prompt });
    expect(group.querySelector("legend")?.classList.contains("sr-only")).toBe(true);
    expect(container.querySelectorAll("legend:not(.sr-only)").length).toBe(0);
    const select = screen.getByRole("combobox", { name: request.prompt });
    expect(within(select).getByRole("option", { name: copy.placeholder })).toBeTruthy();
    expect(within(select).getByRole("option", { name: copy.other })).toBeTruthy();
    expect(screen.getByText("Only the selected answer is submitted.")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: copy.submit }));
    expect(screen.getByRole("alert").textContent).toBe(copy.required(request.prompt));
    expect(onDecision).not.toHaveBeenCalled();
    await user.selectOptions(select, "read_only");
    expect(screen.queryByRole("alert")).toBeNull();
    await user.click(screen.getByRole("button", { name: copy.submit }));
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({
      kind: "submitted",
      answers: [{ questionId: "acceptance", value: { kind: "single_select", optionId: "read_only" } }],
    });
  });

  it("keeps distinct request and field wording visible without translating model content", () => {
    const request = requestedRequest();
    request.prompt = "Choose an acceptance method";
    const { container } = renderRequested(request, "zh-CN");
    expect(screen.getByRole("heading", { name: request.prompt })).toBeTruthy();
    expect(container.querySelector("legend:not(.sr-only)")?.textContent).toBe(request.questions[0].question);
    expect(screen.getByRole("combobox", { name: request.questions[0].question })).toBeTruthy();
    expect(screen.getByText("Only the selected answer is submitted.")).toBeTruthy();
  });

  it("keeps every field legend visible for multiple questions even when one matches the request", () => {
    const request = requestedRequest();
    request.questions.push({ id: "notes", question: "附加要求", required: false, multiple: false, options: [] });
    const { container } = renderRequested(request);
    expect([...container.querySelectorAll("legend:not(.sr-only)")].map((legend) => legend.textContent))
      .toEqual([request.prompt, "附加要求"]);
    expect(screen.getByRole("textbox", { name: "附加要求" })).toBeTruthy();
  });

  it.each([
    ["en", "text"], ["zh-CN", "text"], ["en", "multiple"], ["zh-CN", "multiple"],
  ] as const)("keeps required %s %s validation and exact successful answer values", async (locale, kind) => {
    const user = userEvent.setup();
    const request = requestedRequest();
    request.questions[0] = { ...request.questions[0], options: kind === "text" ? [] : request.questions[0].options, multiple: kind === "multiple" };
    const { onDecision } = renderRequested(request, locale);
    await user.click(screen.getByRole("button", { name: localized[locale].submit }));
    expect(screen.getByRole("alert").textContent).toBe(localized[locale].required(request.prompt));
    expect(onDecision).not.toHaveBeenCalled();
    const original = "  中文补充  ";
    if (kind === "text") {
      await user.type(screen.getByRole("textbox", { name: request.prompt }), original);
    } else {
      await user.click(screen.getByRole("checkbox", { name: "只读检查" }));
      await user.type(screen.getByRole("textbox", { name: request.prompt + localized[locale].otherLabel }), original);
    }
    await user.click(screen.getByRole("button", { name: localized[locale].submit }));
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({
      kind: "submitted",
      answers: [{ questionId: "acceptance", value: kind === "text"
        ? { kind: "text", value: original }
        : { kind: "multi_select", optionIds: ["read_only"], other: original } }],
    });
  });

  it.each(["en", "zh-CN"] as const)("requires an Other value in %s and preserves the entered text", async (locale) => {
    const user = userEvent.setup();
    const request = requestedRequest();
    const { onDecision } = renderRequested(request, locale);
    await user.selectOptions(screen.getByRole("combobox", { name: request.prompt }), "other");
    await user.click(screen.getByRole("button", { name: localized[locale].submit }));
    expect(screen.getByRole("alert").textContent).toBe(localized[locale].otherRequired(request.prompt));
    expect(onDecision).not.toHaveBeenCalled();
    await user.type(screen.getByRole("textbox", { name: request.prompt + localized[locale].otherLabel }), "  原文  ");
    await user.click(screen.getByRole("button", { name: localized[locale].submit }));
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({
      kind: "submitted", answers: [{ questionId: "acceptance", value: { kind: "single_select", other: "  原文  " } }],
    });
  });

  it("continues to omit unanswered optional text, multi-select, and Other fields", async () => {
    const user = userEvent.setup();
    const request = requestedRequest();
    request.questions = [
      { ...request.questions[0], required: false },
      { id: "notes", question: "Notes", required: false, options: [], multiple: false },
      { ...request.questions[0], id: "multi", question: "Formats", required: false, multiple: true },
    ];
    const { onDecision } = renderRequested(request);
    await user.selectOptions(screen.getByRole("combobox", { name: request.prompt }), "other");
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({ kind: "submitted", answers: [] });
  });

  it("relocalizes an existing typed validation issue without changing request or answer state", async () => {
    function ChangeLocale() {
      const { setLocale } = useLocale();
      return <button onClick={() => setLocale("zh-CN")}>Change language</button>;
    }
    const user = userEvent.setup();
    const request = requestedRequest();
    const onDecision = vi.fn();
    render(<LocaleProvider><ChangeLocale /><UserInputCard request={request} busy={false} failure={false} onDecision={onDecision} /></LocaleProvider>);
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));
    expect(screen.getByRole("alert").textContent).toBe(localized.en.required(request.prompt));
    await user.click(screen.getByRole("button", { name: "Change language" }));
    expect(screen.getByRole("alert").textContent).toBe(localized["zh-CN"].required(request.prompt));
    await user.selectOptions(screen.getByRole("combobox", { name: request.prompt }), "read_only");
    await user.click(screen.getByRole("button", { name: "提交并继续" }));
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({ kind: "submitted", answers: [{ questionId: "acceptance", value: { kind: "single_select", optionId: "read_only" } }] });
  });
});

describe("UserInputCard recovery", () => {
  it("shows a private-value-free resume action for a durable accepted answer", async () => {
    const user = userEvent.setup();
    const onResume = vi.fn();
    render(
      <LocaleProvider>
        <UserInputCard
          request={acceptedRequest()}
          busy={false}
          failure={false}
          onDecision={vi.fn()}
          onResume={onResume}
        />
      </LocaleProvider>,
    );

    expect(screen.queryByRole("textbox", { name: "Workspace" })).toBeNull();
    expect(screen.getByRole("status").textContent).toBe("Answer saved. Continue without answering again.");
    expect(screen.queryByText(/Answered fields:/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(onResume).toHaveBeenCalledTimes(1);
  });

  it("surfaces bounded attention queue navigation without changing request authority", async () => {
    const user = userEvent.setup();
    const onPrevious = vi.fn();
    const onNext = vi.fn();
    render(
      <LocaleProvider>
        <UserInputCard
          request={acceptedRequest()}
          busy={false}
          failure={false}
          queuePosition={2}
          queueLength={3}
          onPrevious={onPrevious}
          onNext={onNext}
          onDecision={vi.fn()}
          onResume={vi.fn()}
        />
      </LocaleProvider>,
    );

    expect(screen.getByText("Question 2 of 3")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Previous" }));
    await user.click(screen.getByRole("button", { name: "Next" }));
    expect(onPrevious).toHaveBeenCalledTimes(1);
    expect(onNext).toHaveBeenCalledTimes(1);
  });
});


describe("UserInputCard answered-state hierarchy", () => {
  const answeredCopy = {
    en: { saved: "Answer saved. Continue without answering again.", running: "Answer received. Continuing…", required: "Input required", resume: "Continue", previous: "Previous", next: "Next", resuming: "Continuing…" },
    "zh-CN": { saved: "回答已保存，可直接继续，无需重新填写。", running: "已收到回答，正在继续…", required: "需要你的输入", resume: "继续", previous: "上一个", next: "下一个", resuming: "正在继续…" },
  };

  it.each([
    ["en", "continuation_claimed"], ["en", "continuation_started"],
    ["zh-CN", "continuation_claimed"], ["zh-CN", "continuation_started"],
  ] as const)("shows a quiet received state for %s %s, retaining the question's accessible name", (locale, status) => {
    window.localStorage.setItem("sigil.desktop.locale.v1", locale);
    const request = acceptedRequest();
    request.status = status;
    request.questions[0].id = "activity_name";
    request.answerReceipt!.answeredQuestionIds = ["activity_name"];
    const onResume = vi.fn();
    const { container } = render(<LocaleProvider><UserInputCard request={request} busy={false} failure={false} onDecision={vi.fn()} onResume={onResume} /></LocaleProvider>);
    const region = screen.getByRole("region", { name: request.prompt });
    expect(region.classList.contains("user-input-card-answered")).toBe(true);
    expect(within(region).getByRole("status").textContent).toBe(answeredCopy[locale].running);
    expect(within(region).getByRole("heading", { name: request.prompt }).classList.contains("sr-only")).toBe(true);
    expect(screen.queryByText(answeredCopy[locale].required)).toBeNull();
    expect(screen.queryByText(/activity_name/)).toBeNull();
    expect(region.querySelector("header")).toBeNull();
    expect(container.querySelector(".user-input-recovery")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("button", { name: answeredCopy[locale].resume })).toBeNull();
    expect(onResume).not.toHaveBeenCalled();
  });

  it.each(["en", "zh-CN"] as const)("retains saved-answer retry, failure and queue navigation in %s", async (locale) => {
    window.localStorage.setItem("sigil.desktop.locale.v1", locale);
    const user = userEvent.setup();
    const onResume = vi.fn();
    const onDecision = vi.fn();
    const onPrevious = vi.fn();
    const onNext = vi.fn();
    const request = acceptedRequest();
    render(<LocaleProvider><UserInputCard request={request} busy={false} failure queuePosition={2} queueLength={3} onPrevious={onPrevious} onNext={onNext} onDecision={onDecision} onResume={onResume} /></LocaleProvider>);
    expect(screen.getByRole("status").textContent).toBe(answeredCopy[locale].saved);
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.queryByText(answeredCopy[locale].required)).toBeNull();
    await user.click(screen.getByRole("button", { name: answeredCopy[locale].resume }));
    await user.click(screen.getByRole("button", { name: answeredCopy[locale].previous }));
    await user.click(screen.getByRole("button", { name: answeredCopy[locale].next }));
    expect(onResume).toHaveBeenCalledTimes(1);
    expect(onPrevious).toHaveBeenCalledTimes(1);
    expect(onNext).toHaveBeenCalledTimes(1);
    expect(onDecision).not.toHaveBeenCalled();
  });

  it("switches the same question from requested to saved to running without resubmitting or changing the answer", async () => {
    const user = userEvent.setup();
    const request = { ...acceptedRequest(), status: "requested" as const, answerReceipt: undefined };
    const onDecision = vi.fn();
    const onResume = vi.fn();
    const card = (current: UserInputRequest, busy = false) => <LocaleProvider><UserInputCard request={current} busy={busy} failure={false} onDecision={onDecision} onResume={onResume} /></LocaleProvider>;
    const { rerender } = render(card(request));
    expect(screen.getByText("Input required")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));
    expect(screen.getByRole("alert")).toBeTruthy();
    const answer = "  活动名称 A  ";
    await user.type(screen.getByRole("textbox", { name: request.prompt }), answer);
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));
    expect(onDecision).toHaveBeenCalledExactlyOnceWith({ kind: "submitted", answers: [{ questionId: "workspace", value: { kind: "text", value: answer } }] });
    rerender(card(acceptedRequest()));
    expect(screen.getByRole("status").textContent).toBe(answeredCopy.en.saved);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(onResume).toHaveBeenCalledTimes(1);
    rerender(card({ ...acceptedRequest(), status: "continuation_started" }));
    expect(screen.getByRole("status").textContent).toBe(answeredCopy.en.running);
    expect(screen.queryByText("Input required")).toBeNull();
    expect(onDecision).toHaveBeenCalledTimes(1);
  });

  it("keeps a running continuation failure and other pending questions reachable", async () => {
    const user = userEvent.setup();
    const onNext = vi.fn();
    render(<LocaleProvider><UserInputCard request={{ ...acceptedRequest(), status: "continuation_started" }} busy={false} failure queueLength={2} onNext={onNext} onDecision={vi.fn()} onResume={vi.fn()} /></LocaleProvider>);
    expect(screen.getByRole("status").textContent).toBe(answeredCopy.en.running);
    expect(screen.getByRole("alert").textContent).toBe("Could not continue. Reload the latest question and try again.");
    expect(screen.queryByRole("button", { name: "Continue" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Next" }));
    expect(onNext).toHaveBeenCalledTimes(1);
  });

  it("does not re-ask an accepted question because of stale local required-field validation", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    const request = { ...acceptedRequest(), status: "requested" as const, answerReceipt: undefined };
    const card = (current: UserInputRequest) => <LocaleProvider><UserInputCard request={current} busy={false} failure={false} onDecision={onDecision} onResume={vi.fn()} /></LocaleProvider>;
    const { rerender } = render(card(request));
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(onDecision).not.toHaveBeenCalled();
    rerender(card(acceptedRequest()));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("Input required")).toBeNull();
    expect(screen.getByRole("button", { name: "Continue" })).toBeTruthy();
  });

  it("keeps a busy accepted answer's existing resume and queue actions disabled", () => {
    render(<LocaleProvider><UserInputCard request={acceptedRequest()} busy failure={false} queueLength={2} onNext={vi.fn()} onDecision={vi.fn()} onResume={vi.fn()} /></LocaleProvider>);
    expect((screen.getByRole("button", { name: "Continuing…" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("status").textContent).toBe(answeredCopy.en.saved);
  });
});
