import { createRef } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
        clientAction: "preview_compaction",
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
  active?: boolean;
  submissionBlocked?: boolean;
  queueCount?: number;
  queuePaused?: boolean;
  queueBusy?: boolean;
  onSubmit?: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding) => Promise<boolean>;
  onInterruptAndRunNext?: (prompt: string) => Promise<boolean>;
  onOpenQueue?: () => void;
  onReasoningEffortChange?: (effort: ReasoningEffort) => void;
  onOpenAgentWorkbench?: (query: string) => void;
  onOpenSessionPicker?: (query: string) => void;
  onOpenSettings?: () => void;
  onOpenSupport?: () => void;
  onPreviewCompaction?: () => void;
  onNotice?: (message: string, error?: boolean) => void;
} = {}) {
  const onSubmit = overrides.onSubmit ?? vi.fn(async (
    _prompt: string,
    _skillBinding?: SkillBinding,
    _agentBinding?: AgentBinding,
  ) => true);
  const onInterruptAndRunNext = overrides.onInterruptAndRunNext ?? vi.fn(async (_prompt: string) => true);
  const onOpenQueue = overrides.onOpenQueue ?? vi.fn(() => undefined);
  const onReasoningEffortChange = overrides.onReasoningEffortChange ?? vi.fn((_effort: ReasoningEffort) => undefined);
  const onOpenAgentWorkbench = overrides.onOpenAgentWorkbench ?? vi.fn((_query: string) => undefined);
  const onOpenSessionPicker = overrides.onOpenSessionPicker ?? vi.fn((_query: string) => undefined);
  const onOpenSettings = overrides.onOpenSettings ?? vi.fn(() => undefined);
  const onOpenSupport = overrides.onOpenSupport ?? vi.fn(() => undefined);
  const onPreviewCompaction = overrides.onPreviewCompaction ?? vi.fn(() => undefined);
  const onNotice = overrides.onNotice ?? vi.fn((_message: string, _error?: boolean) => undefined);
  render(
    <LocaleProvider>
      <Composer
        draftKey="composer-test"
        active={overrides.active ?? false}
        submissionBlocked={overrides.submissionBlocked ?? false}
        submitting={false}
        controlBusy={false}
        composerRef={createRef<HTMLTextAreaElement>()}
        runContext={context}
        runContextBusy={false}
        selectedModelName={context.modelName}
        permissionMode="manual"
        reasoningEffort="max"
        queueCount={overrides.queueCount ?? 0}
        queuePaused={overrides.queuePaused ?? false}
        queueBusy={overrides.queueBusy ?? false}
        onModelChange={() => undefined}
        onPermissionModeChange={() => undefined}
        onReasoningEffortChange={onReasoningEffortChange}
        onNewSession={async () => true}
        onOpenSessionPicker={onOpenSessionPicker}
        onOpenSettings={onOpenSettings}
        onOpenSupport={onOpenSupport}
        onOpenAgentWorkbench={onOpenAgentWorkbench}
        onOpenQueue={onOpenQueue}
        onPreviewCompaction={onPreviewCompaction}
        onNotice={onNotice}
        onSubmit={onSubmit}
        onInterruptAndRunNext={onInterruptAndRunNext}
        onCancel={() => undefined}
      />
    </LocaleProvider>,
  );
  return {
    onSubmit,
    onInterruptAndRunNext,
    onOpenQueue,
    onReasoningEffortChange,
    onOpenAgentWorkbench,
    onOpenSessionPicker,
    onOpenSettings,
    onOpenSupport,
    onPreviewCompaction,
    onNotice,
  };
}

describe("structured composer", () => {
  it("binds invocation suggestions to the composer with the combobox contract", async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    expect(input.getAttribute("aria-expanded")).toBe("false");
    await user.type(input, "/");

    const listbox = screen.getByRole("listbox", { name: "Composer suggestions" });
    const firstOption = screen.getAllByRole("option")[0];
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(input.getAttribute("aria-controls")).toBe(listbox.id);
    expect(input.getAttribute("aria-activedescendant")).toBe(firstOption.id);

    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).toBe(screen.getAllByRole("option")[1].id);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("keeps the draft editable but blocks run actions until continuity is verified", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ submissionBlocked: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Keep this draft");
    fireEvent.keyDown(input, { key: "Enter" });

    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft");
    expect(onSubmit).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(true);
    for (const select of screen.getAllByRole("combobox").filter((control) => control.tagName === "SELECT")) {
      expect((select as HTMLSelectElement).disabled).toBe(true);
    }
  });

  it("selects an exact skill binding and submits only the task prompt", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

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
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

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
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/effort high");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onReasoningEffortChange).toHaveBeenCalledWith("high");
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("opens and filters the agent workbench from the slash command", async () => {
    const user = userEvent.setup();
    const { onSubmit, onOpenAgentWorkbench } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/agent plan");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenAgentWorkbench).toHaveBeenCalledWith("plan");
    expect(onSubmit).not.toHaveBeenCalled();
    await waitFor(() => expect((input as HTMLTextAreaElement).value).toBe(""));
  });

  it("binds the supervised plan agent from /plan", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/plan inspect the runtime");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("inspect the runtime", undefined, {
      profileId: "plan",
      snapshotId: "plan-snapshot",
    });
  });

  it("routes /resume to conversation search and keeps compact available", async () => {
    const user = userEvent.setup();
    const { onOpenSessionPicker } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/");
    expect(screen.getByRole("option", { name: /Compact context/ })).not.toBeNull();
    await user.clear(input);
    await user.type(input, "/resume typo");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSessionPicker).toHaveBeenCalledWith("typo");
  });

  it("routes /compact to an explicit desktop preview instead of the model", async () => {
    const user = userEvent.setup();
    const { onPreviewCompaction, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/compact");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onPreviewCompaction).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("routes /config to the desktop settings page", async () => {
    const user = userEvent.setup();
    const { onOpenSettings, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/config");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it.each(["/doctor", "/feedback"])("routes %s to support and diagnostics", async (command) => {
    const user = userEvent.setup();
    const { onOpenSupport, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, command);
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSupport).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows unavailable agents without dispatching a run", async () => {
    const user = userEvent.setup();
    const { onSubmit, onNotice } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "@dis");
    await user.click(screen.getByRole("option", { name: /disabled/ }));

    expect(onNotice).toHaveBeenCalledWith(
      "Desktop agent execution requires the supervised child-session owner.",
      true,
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("queues Enter submissions by default while a run is active", async () => {
    const user = userEvent.setup();
    const { onSubmit, onInterruptAndRunNext } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Run this after the current task");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("Run this after the current task", undefined, undefined);
    expect(onInterruptAndRunNext).not.toHaveBeenCalled();
    await waitFor(() => expect((input as HTMLTextAreaElement).value).toBe(""));
  });

  it("does not queue an IME composition Enter", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "继续执行");
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });

    expect(onSubmit).not.toHaveBeenCalled();
    expect((input as HTMLTextAreaElement).value).toBe("继续执行");
  });

  it("requires confirmation before interrupting and clears only after durable success", async () => {
    const user = userEvent.setup();
    const onInterruptAndRunNext = vi.fn(async (_prompt: string) => false);
    renderComposer({ active: true, onInterruptAndRunNext });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Urgent follow-up");
    await user.click(screen.getByRole("button", { name: "Interrupt and run next" }));
    expect(onInterruptAndRunNext).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeTruthy();

    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Interrupt and run next" }));
    expect(onInterruptAndRunNext).toHaveBeenCalledWith("Urgent follow-up");
    expect((input as HTMLTextAreaElement).value).toBe("Urgent follow-up");
  });

  it("opens the durable queue without changing the draft", async () => {
    const user = userEvent.setup();
    const { onOpenQueue } = renderComposer({ active: true, queueCount: 2 });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Keep this draft");
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 2 messages" }));

    expect(onOpenQueue).toHaveBeenCalledOnce();
    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft");
  });
});
