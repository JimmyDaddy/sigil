import { createRef } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Composer } from "./Composer";
import { LocaleProvider } from "./i18n";
import type { ReasoningEffort, RunContext, SkillBinding } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const context: RunContext = {
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  availableModels: ["deepseek-v4-flash", "deepseek-v4-pro"],
  modelSelection: "per_run",
  modelSelectionBinding: "model-binding",
  defaultPermissionMode: "manual",
  availablePermissionModes: ["read-only", "manual", "auto-edit", "danger-full-access"],
  availableReasoningEfforts: ["low", "medium", "high", "max"],
  defaultReasoningEffort: "max",
  reasoningEffortBinding: "effort-binding",
  contextWindowTokens: 1_000_000,
  lastPromptTokens: 1_000,
  contextWindowSource: "provider",
  extensionCatalog: {
    commands: [
      {
        canonical: "/effort",
        aliases: ["/e"],
        label: "Change effort",
        description: "set reasoning effort",
        argumentHint: "<low|medium|high|max>",
        completesWithSpace: true,
        clientAction: "focus_effort",
        available: true,
      },
      {
        canonical: "/new",
        aliases: [],
        label: "New conversation",
        description: "start a fresh session",
        completesWithSpace: false,
        clientAction: "new_session",
        available: true,
      },
      {
        canonical: "/agent",
        aliases: [],
        label: "Agents",
        description: "open the agent workbench",
        argumentHint: "[profile]",
        completesWithSpace: true,
        clientAction: "open_agent_workbench",
        available: true,
      },
    ],
    skills: [
      {
        id: "review",
        invocationToken: "$review",
        name: "Review",
        description: "Review the selected code.",
        source: "workspace",
        runMode: "inline",
        trust: "trusted",
        available: true,
        binding: {
          skillId: "review",
          skillSha256: "skill-sha",
          indexFingerprint: "index-sha",
        },
      },
    ],
    agents: [
      {
        id: "explore",
        invocationToken: "@explore",
        description: "Read-only exploration agent.",
        source: "system",
        kind: "subagent",
        trust: "trusted",
        enabled: true,
        userInvocable: true,
        available: false,
        unavailableReason: "Desktop agent execution requires the supervised child-session owner.",
      },
    ],
  },
};

function renderComposer(overrides: {
  onSubmit?: (prompt: string, binding?: SkillBinding) => Promise<boolean>;
  onReasoningEffortChange?: (effort: ReasoningEffort) => void;
  onOpenAgentWorkbench?: (query: string) => void;
  onNotice?: (message: string, error?: boolean) => void;
} = {}) {
  const onSubmit = overrides.onSubmit ?? vi.fn(async (_prompt: string, _binding?: SkillBinding) => true);
  const onReasoningEffortChange = overrides.onReasoningEffortChange ?? vi.fn((_effort: ReasoningEffort) => undefined);
  const onOpenAgentWorkbench = overrides.onOpenAgentWorkbench ?? vi.fn((_query: string) => undefined);
  const onNotice = overrides.onNotice ?? vi.fn((_message: string, _error?: boolean) => undefined);
  render(
    <LocaleProvider>
      <Composer
        draftKey="composer-test"
        active={false}
        submitting={false}
        controlBusy={false}
        composerRef={createRef<HTMLTextAreaElement>()}
        runContext={context}
        runContextBusy={false}
        selectedModelName={context.modelName}
        permissionMode="manual"
        reasoningEffort="max"
        onModelChange={() => undefined}
        onPermissionModeChange={() => undefined}
        onReasoningEffortChange={onReasoningEffortChange}
        onNewSession={async () => true}
        onOpenAgentWorkbench={onOpenAgentWorkbench}
        onNotice={onNotice}
        onSubmit={onSubmit}
        onCancel={() => undefined}
      />
    </LocaleProvider>,
  );
  return { onSubmit, onReasoningEffortChange, onOpenAgentWorkbench, onNotice };
}

describe("structured composer", () => {
  it("selects an exact skill binding and submits only the task prompt", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "$rev");
    await user.click(screen.getByRole("option", { name: /Review/ }));
    expect(screen.getByText("$review")).toBeTruthy();
    await user.type(input, "inspect src/lib.rs");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("inspect src/lib.rs", {
      skillId: "review",
      skillSha256: "skill-sha",
      indexFingerprint: "index-sha",
    });
  });

  it("routes slash effort locally instead of sending it to the model", async () => {
    const user = userEvent.setup();
    const { onSubmit, onReasoningEffortChange } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "/effort high");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onReasoningEffortChange).toHaveBeenCalledWith("high");
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("opens and filters the agent workbench from the slash command", async () => {
    const user = userEvent.setup();
    const { onSubmit, onOpenAgentWorkbench } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "/agent plan");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenAgentWorkbench).toHaveBeenCalledWith("plan");
    expect(onSubmit).not.toHaveBeenCalled();
    await waitFor(() => expect((input as HTMLTextAreaElement).value).toBe(""));
  });

  it("shows unavailable agents without dispatching a run", async () => {
    const user = userEvent.setup();
    const { onSubmit, onNotice } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "@expl");
    await user.click(screen.getByRole("option", { name: /explore/ }));

    expect(onNotice).toHaveBeenCalledWith(
      "Desktop agent execution requires the supervised child-session owner.",
      true,
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
