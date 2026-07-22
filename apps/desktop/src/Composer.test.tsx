import { createRef } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Composer } from "./Composer";
import { LocaleProvider } from "./i18n";
import type { AgentBinding, ReasoningEffort, RunContext, SkillBinding } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const context: RunContext = {
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  availableModels: ["deepseek-v4-flash", "deepseek-v4-pro"],
  modelOptions: [
    {
      modelName: "deepseek-v4-flash",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding",
    },
    {
      modelName: "deepseek-v4-pro",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-pro",
    },
  ],
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
        canonical: "/config",
        aliases: [],
        label: "Open settings",
        description: "edit config",
        completesWithSpace: false,
        clientAction: "open_settings",
        available: true,
      },
      {
        canonical: "/doctor",
        aliases: [],
        label: "Run diagnostics",
        description: "run local diagnostics",
        completesWithSpace: false,
        clientAction: "open_support",
        available: true,
      },
      {
        canonical: "/feedback",
        aliases: [],
        label: "Send feedback",
        description: "review and export a private support report",
        completesWithSpace: false,
        clientAction: "open_support",
        available: true,
      },
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
      {
        canonical: "/plan",
        aliases: [],
        label: "Plan",
        description: "enter plan mode or run one plan prompt",
        argumentHint: "[prompt]",
        completesWithSpace: true,
        clientAction: "open_agent_workbench",
        available: true,
      },
      {
        canonical: "/resume",
        aliases: [],
        label: "Open conversation",
        description: "choose a saved session",
        argumentHint: "[query]",
        completesWithSpace: true,
        clientAction: "open_session_picker",
        available: true,
      },
      {
        canonical: "/compact",
        aliases: [],
        label: "Compact context",
        description: "preview V2 context compaction",
        completesWithSpace: false,
        available: false,
        unavailableReason: "This command does not yet have a desktop application route.",
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
        available: true,
        binding: {
          profileId: "explore",
          snapshotId: "agent-snapshot",
        },
      },
      {
        id: "disabled",
        invocationToken: "@disabled",
        description: "Disabled agent.",
        source: "workspace",
        kind: "subagent",
        trust: "trusted",
        enabled: false,
        userInvocable: true,
        available: false,
        unavailableReason: "Desktop agent execution requires the supervised child-session owner.",
      },
      {
        id: "plan",
        invocationToken: "@plan",
        description: "Planning agent.",
        source: "system",
        kind: "primary",
        trust: "trusted",
        enabled: true,
        userInvocable: true,
        available: true,
        binding: {
          profileId: "plan",
          snapshotId: "plan-snapshot",
        },
      },
    ],
  },
};

function renderComposer(overrides: {
  onSubmit?: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding) => Promise<boolean>;
  onReasoningEffortChange?: (effort: ReasoningEffort) => void;
  onOpenAgentWorkbench?: (query: string) => void;
  onOpenSessionPicker?: (query: string) => void;
  onOpenSettings?: () => void;
  onOpenSupport?: () => void;
  onNotice?: (message: string, error?: boolean) => void;
} = {}) {
  const onSubmit = overrides.onSubmit ?? vi.fn(async (
    _prompt: string,
    _skillBinding?: SkillBinding,
    _agentBinding?: AgentBinding,
  ) => true);
  const onReasoningEffortChange = overrides.onReasoningEffortChange ?? vi.fn((_effort: ReasoningEffort) => undefined);
  const onOpenAgentWorkbench = overrides.onOpenAgentWorkbench ?? vi.fn((_query: string) => undefined);
  const onOpenSessionPicker = overrides.onOpenSessionPicker ?? vi.fn((_query: string) => undefined);
  const onOpenSettings = overrides.onOpenSettings ?? vi.fn(() => undefined);
  const onOpenSupport = overrides.onOpenSupport ?? vi.fn(() => undefined);
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
        onOpenSessionPicker={onOpenSessionPicker}
        onOpenSettings={onOpenSettings}
        onOpenSupport={onOpenSupport}
        onOpenAgentWorkbench={onOpenAgentWorkbench}
        onNotice={onNotice}
        onSubmit={onSubmit}
        onCancel={() => undefined}
      />
    </LocaleProvider>,
  );
  return { onSubmit, onReasoningEffortChange, onOpenAgentWorkbench, onOpenSessionPicker, onOpenSettings, onOpenSupport, onNotice };
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
    }, undefined);
  });

  it("selects an exact agent snapshot and submits only the task prompt", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "@expl");
    await user.click(screen.getByRole("option", { name: /explore/ }));
    await user.type(input, "inspect src/lib.rs");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("inspect src/lib.rs", undefined, {
      profileId: "explore",
      snapshotId: "agent-snapshot",
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

  it("binds the supervised plan agent from /plan", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "/plan inspect the runtime");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("inspect the runtime", undefined, {
      profileId: "plan",
      snapshotId: "plan-snapshot",
    });
  });

  it("routes /resume to conversation search and hides unsupported commands", async () => {
    const user = userEvent.setup();
    const { onOpenSessionPicker } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "/");
    expect(screen.queryByRole("option", { name: /Compact context/ })).toBeNull();
    await user.clear(input);
    await user.type(input, "/resume typo");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSessionPicker).toHaveBeenCalledWith("typo");
  });

  it("routes /config to the desktop settings page", async () => {
    const user = userEvent.setup();
    const { onOpenSettings, onSubmit } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "/config");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it.each(["/doctor", "/feedback"])("routes %s to support and diagnostics", async (command) => {
    const user = userEvent.setup();
    const { onOpenSupport, onSubmit } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, command);
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSupport).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows unavailable agents without dispatching a run", async () => {
    const user = userEvent.setup();
    const { onSubmit, onNotice } = renderComposer();
    const input = screen.getByRole("textbox");

    await user.type(input, "@dis");
    await user.click(screen.getByRole("option", { name: /disabled/ }));

    expect(onNotice).toHaveBeenCalledWith(
      "Desktop agent execution requires the supervised child-session owner.",
      true,
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
