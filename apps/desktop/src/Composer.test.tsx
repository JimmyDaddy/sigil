import { createRef, type ReactNode } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Composer } from "./Composer";
import type { ComposerActivityState } from "./features/conversation/composerActivity";
import { LocaleProvider } from "./i18n";
import type {
  AgentBinding,
  ProviderConnection,
  ProviderModelRef,
  ReasoningEffort,
  RunContext,
  SkillBinding,
} from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const context: RunContext = {
  modelRef: {
    connectionId: "deepseek-default",
    modelId: "deepseek-v4-flash",
  },
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  modelOptions: [
    {
      modelRef: { connectionId: "deepseek-default", modelId: "deepseek-v4-flash" },
      displayName: "DeepSeek V4 Flash",
      availability: "available",
      recommendation: "recommended",
      provenance: "bundled",
      modelName: "deepseek-v4-flash",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding",
    },
    {
      modelRef: { connectionId: "deepseek-default", modelId: "deepseek-v4-pro" },
      displayName: "DeepSeek V4 Pro",
      availability: "available",
      recommendation: "standard",
      provenance: "bundled",
      modelName: "deepseek-v4-pro",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-pro",
    },
  ],
  modelSelection: "fresh_session",
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
      {
        canonical: "/intents",
        aliases: [],
        label: "Intent Stack",
        description: "review durable intents",
        completesWithSpace: false,
        clientAction: "open_intent_stack",
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
      {
        id: "compat-command-continue",
        invocationToken: "/继续写",
        name: "继续写",
        description: "Continue drafting.",
        source: "workspace",
        runMode: "inline",
        trust: "trusted",
        available: true,
        binding: {
          skillId: "compat-command-continue",
          skillSha256: "continue-sha",
          indexFingerprint: "index-sha",
        },
      },
    ],
    agents: [
      {
        id: "explore",
        invocationToken: "@explore",
        name: "Explore",
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
        name: "Disabled",
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
        name: "Plan",
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
      {
        id: "compat-agent-planner",
        invocationToken: "@正典提升员",
        name: "正典提升员",
        description: "Improve story canon.",
        source: "compatibility",
        kind: "subagent",
        trust: "trusted",
        enabled: true,
        userInvocable: true,
        available: true,
        binding: {
          profileId: "compat-agent-planner",
          snapshotId: "planner-snapshot",
        },
      },
    ],
  },
};

function renderComposer(overrides: {
  active?: boolean;
  submissionBlocked?: boolean;
  queueSubmissionBlocked?: boolean;
  queueCount?: number;
  queuePaused?: boolean;
  queueBusy?: boolean;
  queuePanel?: ReactNode;
  onSubmit?: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding) => Promise<boolean>;
  onInterruptAndRunNext?: (prompt: string) => Promise<boolean>;
  onOpenQueue?: () => void;
  onReasoningEffortChange?: (effort: ReasoningEffort) => void;
  onOpenAgentWorkbench?: (query: string) => void;
  onOpenSessionPicker?: (query: string) => void;
  onOpenSettings?: () => void;
  onOpenSupport?: () => void;
  onCompact?: () => Promise<boolean>;
  onOpenIntentStack?: () => void;
  onNotice?: (message: string, error?: boolean) => void;
  activityState?: ComposerActivityState;
  runContext?: RunContext;
  providerConnections?: ProviderConnection[];
  onModelChange?: (modelRef: ProviderModelRef) => void;
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
  const onCompact = overrides.onCompact ?? vi.fn(async () => true);
  const onOpenIntentStack = overrides.onOpenIntentStack ?? vi.fn(() => undefined);
  const onNotice = overrides.onNotice ?? vi.fn((_message: string, _error?: boolean) => undefined);
  const runContext = overrides.runContext ?? context;
  const onModelChange =
    overrides.onModelChange ?? vi.fn((_modelRef: ProviderModelRef) => undefined);
  const providerConnections = overrides.providerConnections ?? [{
    id: "deepseek-default",
    label: "DeepSeek 1",
    providerLabel: "DeepSeek",
    protocolLabel: "DeepSeek",
    endpointDisplay: "api.deepseek.com",
    credentialSource: "environment",
    readiness: "ready",
  }];
  render(
    <LocaleProvider>
      <Composer
        draftKey="composer-test"
        active={overrides.active ?? false}
        submissionBlocked={overrides.submissionBlocked ?? false}
        queueSubmissionBlocked={overrides.queueSubmissionBlocked ?? false}
        submitting={false}
        controlBusy={false}
        composerRef={createRef<HTMLTextAreaElement>()}
        runContext={runContext}
        runContextBusy={false}
        providerConnections={providerConnections}
        selectedModelRef={runContext.modelRef}
        permissionMode="manual"
        reasoningEffort="max"
        queueCount={overrides.queueCount ?? 0}
        queuePaused={overrides.queuePaused ?? false}
        queueBusy={overrides.queueBusy ?? false}
        activityState={overrides.activityState}
        queuePanel={overrides.queuePanel}
        onModelChange={onModelChange}
        onPermissionModeChange={() => undefined}
        onReasoningEffortChange={onReasoningEffortChange}
        onNewSession={async () => true}
        onOpenSessionPicker={onOpenSessionPicker}
        onOpenSettings={onOpenSettings}
        onOpenSupport={onOpenSupport}
        onOpenAgentWorkbench={onOpenAgentWorkbench}
        onOpenQueue={onOpenQueue}
        onCompact={onCompact}
        onOpenIntentStack={onOpenIntentStack}
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
    onCompact,
    onOpenIntentStack,
    onNotice,
    onModelChange,
  };
}

describe("structured composer", () => {
  it("labels provider-unconfirmed exact models and keeps them selectable", async () => {
    const onModelChange = vi.fn();
    const unverifiedContext: RunContext = {
      ...context,
      modelOptions: context.modelOptions.map((option) =>
        option.modelName === "deepseek-v4-pro"
          ? { ...option, availability: "unverified", provenance: "bundled" }
          : option,
      ),
    };
    renderComposer({ runContext: unverifiedContext, onModelChange });

    const model = screen.getByRole("combobox", { name: "Model" });
    const unverified = within(model).getByRole("option", {
      name: /DeepSeek 1.*DeepSeek V4 Pro.*Not confirmed by provider/,
    }) as HTMLOptionElement;
    expect(unverified.disabled).toBe(false);

    await userEvent.setup().selectOptions(
      model,
      "deepseek-default/deepseek-v4-pro",
    );
    expect(onModelChange).toHaveBeenCalledWith({
      connectionId: "deepseek-default",
      modelId: "deepseek-v4-pro",
    });
  });

  it("keeps the exact connection and provider visible in the model route", async () => {
    renderComposer({
      providerConnections: [{
        id: "deepseek-default",
        label: "DeepSeek 1",
        providerLabel: "DeepSeek",
        protocolLabel: "DeepSeek",
        endpointDisplay: "api.deepseek.com",
        credentialSource: "environment",
        readiness: "ready",
      }],
    });

    const model = screen.getByRole("combobox", { name: "Model" });
    expect(within(model).getByRole("option", {
      name: /DeepSeek 1.*DeepSeek V4 Flash.*deepseek-v4-flash/,
    })).toBeTruthy();

    const tooltipAnchor = model.closest(".sg-tooltip-anchor");
    expect(tooltipAnchor).not.toBeNull();
    fireEvent.pointerEnter(tooltipAnchor!);
    expect((await screen.findByRole("tooltip")).textContent).toContain(
      "Connection DeepSeek 1 · Provider DeepSeek · Model deepseek-v4-flash",
    );
  });

  it("selects an exact cross-provider route even when model ids are identical", async () => {
    const onModelChange = vi.fn();
    const sameIdContext: RunContext = {
      ...context,
      modelOptions: [
        context.modelOptions[0],
        {
          ...context.modelOptions[0],
          modelRef: { connectionId: "gateway-team", modelId: "deepseek-v4-flash" },
          displayName: "Gateway Flash",
          provenance: "remote",
        },
      ],
    };
    renderComposer({
      runContext: sameIdContext,
      onModelChange,
      providerConnections: [
        {
          id: "deepseek-default",
          label: "DeepSeek 1",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "environment",
          readiness: "ready",
        },
        {
          id: "gateway-team",
          label: "Team gateway",
          providerLabel: "OpenAI-compatible",
          protocolLabel: "Chat Completions",
          endpointDisplay: "gateway.example",
          credentialSource: "stored",
          readiness: "ready",
        },
      ],
    });

    const model = screen.getByRole("combobox", { name: "Model" });
    expect(within(model).getByRole("option", {
      name: /OpenAI-compatible.*Team gateway.*Gateway Flash.*deepseek-v4-flash/,
    })).toBeTruthy();
    await userEvent.setup().selectOptions(model, "gateway-team/deepseek-v4-flash");
    expect(onModelChange).toHaveBeenCalledWith({
      connectionId: "gateway-team",
      modelId: "deepseek-v4-flash",
    });
  });

  it("keeps the current task state visible above the composer", () => {
    renderComposer({ active: true, activityState: "running" });

    const status = screen.getByRole("status");
    expect(within(status).getByText("Sigil is working")).toBeTruthy();
    expect(within(status).getByText("Live output is updating. New messages will be queued.")).toBeTruthy();
  });

  it("focuses the editor without asking WebKit to reveal it by scrolling", () => {
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const focus = vi.spyOn(input, "focus");

    fireEvent.pointerDown(input);

    expect(focus).toHaveBeenCalledOnce();
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  });

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

    const secondOption = screen.getAllByRole("option")[1];
    Object.defineProperty(listbox, "clientHeight", { configurable: true, value: 80 });
    Object.defineProperty(secondOption, "offsetTop", { configurable: true, value: 96 });
    Object.defineProperty(secondOption, "offsetHeight", { configurable: true, value: 48 });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).toBe(secondOption.id);
    expect(listbox.scrollTop).toBe(64);
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

  it("invokes a workspace compatibility command through its human slash token", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/继续写");
    expect(screen.getByRole("option", { name: /Continue drafting/ })).toBeTruthy();
    await user.type(input, " 下一章");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("下一章", {
      skillId: "compat-command-continue",
      skillSha256: "continue-sha",
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

  it("invokes a workspace compatibility agent through its human mention token", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "@正典提升员 检查本章");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("检查本章", undefined, {
      profileId: "compat-agent-planner",
      snapshotId: "planner-snapshot",
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

  it("routes /compact to the direct compaction action instead of the model", async () => {
    const user = userEvent.setup();
    const { onCompact, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/compact");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onCompact).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("routes /intents to the bounded Intent Stack instead of the model", async () => {
    const user = userEvent.setup();
    const { onOpenIntentStack, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "/intents");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenIntentStack).toHaveBeenCalledOnce();
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

  it("keeps the durable queue available while live controls are recovering", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true, submissionBlocked: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Keep working after the current task");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith(
      "Keep working after the current task",
      undefined,
      undefined,
    );
    await waitFor(() => expect((input as HTMLTextAreaElement).value).toBe(""));
  });

  it("retains the draft when the durable queue itself is unavailable", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({
      active: true,
      submissionBlocked: true,
      queueSubmissionBlocked: true,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Do not lose this draft");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).not.toHaveBeenCalled();
    expect((input as HTMLTextAreaElement).value).toBe("Do not lose this draft");
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
    const { onOpenQueue } = renderComposer({
      active: true,
      queueCount: 2,
      queuePanel: <span>Queued prompt controls</span>,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    await user.type(input, "Keep this draft");
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 2 messages" }));

    expect(onOpenQueue).toHaveBeenCalledOnce();
    expect(screen.getByRole("dialog", { name: "Open follow-up queue, 2 messages" })).toBeTruthy();
    expect(screen.getByText("Queued prompt controls")).toBeTruthy();
    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft");
  });
});
