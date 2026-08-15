import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "./i18n";
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
      header: "Workspace",
      question: "Which workspace should Sigil inspect?",
      required: true,
      field: { kind: "text", multiline: false, maxChars: 512 },
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
    expect(screen.getByText(/Answered fields: workspace/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Resume saved answer" }));
    expect(onResume).toHaveBeenCalledTimes(1);
  });
});
