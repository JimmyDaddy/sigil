import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createRef, type ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { editorText, editorView, setEditorText, typeEditorText } from "./test/editor";
import { Composer } from "./Composer";
import { ConversationQueuePanel } from "./ConversationQueuePanel";
import type { ComposerActivityState } from "./features/conversation/composerActivity";
import { LocaleProvider, translateEnglish } from "./i18n";
import { presentComposerActivity, type ProductStatusPresentation } from "./statusPresentation";
import type {
  AgentBinding,
  ConversationQueueCommandAction,
  ConversationQueueView,
  DraftImage,
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
  modelSelection: "same_session",
  modelSelectionBinding: "model-binding",
  defaultPermissionMode: "manual",
  availablePermissionModes: ["read-only", "manual", "auto-edit", "danger-full-access"],
  availableReasoningEfforts: ["low", "medium", "high", "max"],
  defaultReasoningEffort: "max",
  reasoningEffortBinding: "effort-binding",
  contextWindowTokens: 1_000_000,
  lastPromptTokens: 1_000,
  cacheUsage: {
    cacheReadTokens: 900,
    cacheMissTokens: 100,
    cacheWriteTokens: 25,
    lastLayoutMutation: "conversation_tail_appended",
    providerMissWithoutLocalMutation: false,
  },
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
  draftKey?: string;
  conversationHistory?: "empty" | "populated";
  timelineSupplement?: ReactNode;
  persistentDraftKey?: string;
  active?: boolean;
  reviewActive?: boolean;
  submissionBlocked?: boolean;
  queueCount?: number;
  queuePaused?: boolean;
  queueBusy?: boolean;
  queuePanel?: ReactNode;
  onSubmit?: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding, images?: DraftImage[]) => Promise<boolean>;
  onPickImage?: () => Promise<DraftImage | null>;
  onIngestImage?: (bytes: number[]) => Promise<DraftImage>;
  onReleaseImages?: (handles: string[]) => Promise<void>;
  onCancel?: () => void;
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
  statusPresentation?: ProductStatusPresentation;
  runContext?: RunContext;
  providerConnections?: ProviderConnection[];
  onModelChange?: (modelRef: ProviderModelRef) => void;
} = {}) {
  const onSubmit = overrides.onSubmit ?? vi.fn(async (
    _prompt: string,
    _skillBinding?: SkillBinding,
    _agentBinding?: AgentBinding,
  ) => true);
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
  const content = (persistentDraftKey = overrides.persistentDraftKey, reviewActive = overrides.reviewActive) => {
    const composer = (
    <LocaleProvider>
      <Composer
        draftKey={overrides.draftKey ?? "composer-test"}
        persistentDraftKey={persistentDraftKey}
        active={overrides.active ?? false}
        reviewActive={reviewActive}
        submissionBlocked={overrides.submissionBlocked ?? false}
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
        statusPresentation={overrides.statusPresentation}
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
        onPickImage={overrides.onPickImage}
        onIngestImage={overrides.onIngestImage}
        onReleaseImages={overrides.onReleaseImages}
        onCancel={overrides.onCancel ?? (() => undefined)}
      />
    </LocaleProvider>
    );
    return overrides.conversationHistory === undefined ? composer : (
      <div className="conversation-layout"><section className="conversation-panel">
        <header className="conversation-header">Conversation</header>
        <div className="timeline">{overrides.timelineSupplement}{overrides.conversationHistory === "empty"
          ? <div className="timeline-empty">Start a conversation</div>
          : <article className="timeline-row">Previous response</article>}</div>
        {composer}
      </section></div>
    );
  };
  const rendered = render(content());
  return {
    ...rendered,
    bindPersistentDraftKey: (key: string) => rendered.rerender(content(key)),
    bindReviewActive: (active: boolean) => rendered.rerender(content(overrides.persistentDraftKey, active)),
    onSubmit,
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
  it("keeps provider-neutral cache diagnostics in the context detail popover", async () => {
    renderComposer();
    expect(screen.queryByLabelText("Prompt cache hit ratio 90%")).toBeNull();
    await userEvent.setup().click(screen.getByRole("button", { name: "Context and cache details" }));
    const cache = screen.getByLabelText("Prompt cache hit ratio 90%");
    expect(cache.textContent).toBe("Cache 90%");
    const tooltipAnchor = cache.closest(".sg-tooltip-anchor");
    expect(tooltipAnchor).not.toBeNull();
    fireEvent.pointerEnter(tooltipAnchor!);
    expect((await screen.findByRole("tooltip")).textContent).toContain(
      "Cache read 900 · miss 100 · write 25 · layout conversation_tail_appended",
    );
  });

  it("explains a provider-side miss when the local stable prefix did not change", async () => {
    renderComposer({
      runContext: {
        ...context,
        cacheUsage: {
          ...context.cacheUsage!,
          providerMissWithoutLocalMutation: true,
        },
      },
    });
    await userEvent.setup().click(screen.getByRole("button", { name: "Context and cache details" }));
    const cache = screen.getByLabelText("Prompt cache hit ratio 90%");
    expect(cache.classList.contains("cache-warning")).toBe(true);
    const tooltipAnchor = cache.closest(".sg-tooltip-anchor");
    expect(tooltipAnchor).not.toBeNull();
    fireEvent.pointerEnter(tooltipAnchor!);
    expect((await screen.findByRole("tooltip")).textContent).toContain(
      "Provider reported a cache miss although the local stable prefix did not change",
    );
  });

  it("omits unknown context metadata while keeping the prompt and model usable", () => {
    renderComposer({ runContext: { ...context, lastPromptTokens: undefined, contextWindowTokens: undefined, cacheUsage: undefined } });
    expect(screen.queryByText("Context —")).toBeNull();
    expect(screen.queryByRole("button", { name: "Context and cache details" })).toBeNull();
    expect(screen.getByRole("combobox", { name: "Message Sigil" })).toBeTruthy();
    expect((screen.getByRole("combobox", { name: "Model" }) as HTMLSelectElement).disabled).toBe(false);
  });

  it("keeps observed cache diagnostics available when context capacity is unknown", async () => {
    renderComposer({ runContext: { ...context, contextWindowTokens: undefined } });
    await userEvent.setup().click(screen.getByRole("button", { name: "Context and cache details" }));
    expect(screen.queryByRole("meter")).toBeNull();
    expect(screen.getByLabelText("Prompt cache hit ratio 90%")).toBeTruthy();
    expect(screen.queryByText("Context —")).toBeNull();
  });

  it.each(["unverified", "configured_unavailable"] as const)("keeps %s models selectable", async (availability) => {
    const onModelChange = vi.fn();
    const unverifiedContext: RunContext = {
      ...context,
      modelOptions: context.modelOptions.map((option) =>
        option.modelName === "deepseek-v4-pro"
          ? { ...option, availability, provenance: "bundled" }
          : option,
      ),
    };
    renderComposer({ runContext: unverifiedContext, onModelChange });

    const model = screen.getByRole("combobox", { name: "Model" });
    const unverified = within(model).getByRole("option", {
      name: /deepseek-v4-pro/,
    }) as HTMLOptionElement;
    expect(unverified.disabled).toBe(false);
    expect(unverified.textContent).not.toMatch(/Not confirmed by provider/);

    await userEvent.setup().selectOptions(
      model,
      "deepseek-default/deepseek-v4-pro",
    );
    expect(onModelChange).toHaveBeenCalledWith({
      connectionId: "deepseek-default",
      modelId: "deepseek-v4-pro",
    });
  });

  it.each(["needs_credential", "credential_unavailable", "invalid"] as const)(
    "keeps a configured model selectable when inventory readiness is %s",
    async (readiness) => {
      const onModelChange = vi.fn();
      renderComposer({
        onModelChange,
        providerConnections: [{
          id: "deepseek-default",
          label: "DeepSeek 1",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "environment",
          readiness,
        }],
      });

      const model = screen.getByRole("combobox", { name: "Model" });
      const pro = within(model).getByRole("option", { name: /deepseek-v4-pro/ }) as HTMLOptionElement;
      expect(pro.disabled).toBe(false);
      await userEvent.setup().selectOptions(model, "deepseek-default/deepseek-v4-pro");
      expect(onModelChange).toHaveBeenCalledWith({
        connectionId: "deepseek-default", modelId: "deepseek-v4-pro",
      });
    },
  );

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
      name: /deepseek-v4-flash/,
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
      name: /Team gateway.*deepseek-v4-flash/,
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

  it.each([false, true])("preserves edits made while a submission is pending (accepted=%s)", async (accepted) => {
    const user = userEvent.setup();
    let settle: (accepted: boolean) => void = () => undefined;
    const pending = new Promise<boolean>((resolve) => { settle = resolve; });
    const onSubmit = vi.fn(() => pending);
    renderComposer({ onSubmit });
    const input = screen.getByRole("combobox", { name: "Message Sigil" }) as HTMLElement;
    typeEditorText(input, "First request");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onSubmit).toHaveBeenCalledWith("First request", undefined, undefined);

    setEditorText(input, "");
    typeEditorText(input, "Next request written while waiting");
    await act(async () => { settle(accepted); await pending; });

    expect(editorText(input)).toBe("Next request written while waiting");
    expect(window.localStorage.getItem("composer-test")).toBe("Next request written while waiting");
  });

  it("renders the parent supplied typed status presentation", () => {
    renderComposer({
      active: true,
      activityState: "reconnecting",
      statusPresentation: presentComposerActivity("reconnecting", translateEnglish),
    });

    expect(document.querySelector(".composer")?.getAttribute("data-composer-product-status"))
      .toBe("uncertain");
    const status = screen.getByRole("status");
    expect(within(status).getByText("Reconnecting to the task")).toBeTruthy();
    expect(within(status).getByText("Execution may continue while live updates reconnect.")).toBeTruthy();
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
    typeEditorText(input, "/");

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

    typeEditorText(input, "Keep this draft");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(editorText(input)).toBe("Keep this draft");
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

    typeEditorText(input, "$rev");
    await user.click(screen.getByRole("option", { name: /Review/ }));
    expect(screen.getByText("$review")).toBeTruthy();
    typeEditorText(input, "inspect src/lib.rs");
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

    typeEditorText(input, "/继续写");
    expect(screen.getByRole("option", { name: /Continue drafting/ })).toBeTruthy();
    typeEditorText(input, " 下一章");
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

    typeEditorText(input, "@expl");
    await user.click(screen.getByRole("option", { name: /explore/ }));
    typeEditorText(input, "inspect src/lib.rs");
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

    typeEditorText(input, "@正典提升员 检查本章");
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

    typeEditorText(input, "/effort high");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onReasoningEffortChange).toHaveBeenCalledWith("high");
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("opens and filters the agent workbench from the slash command", async () => {
    const user = userEvent.setup();
    const { onSubmit, onOpenAgentWorkbench } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "/agent plan");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenAgentWorkbench).toHaveBeenCalledWith("plan");
    expect(onSubmit).not.toHaveBeenCalled();
    await waitFor(() => expect(editorText(input)).toBe(""));
  });

  it("binds the supervised plan agent from /plan", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "/plan inspect the runtime");
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

    typeEditorText(input, "/");
    expect(screen.getByRole("option", { name: /Compact context/ })).not.toBeNull();
    setEditorText(input, "");
    typeEditorText(input, "/resume typo");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSessionPicker).toHaveBeenCalledWith("typo");
  });

  it("routes /compact to the direct compaction action instead of the model", async () => {
    const user = userEvent.setup();
    const { onCompact, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "/compact");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onCompact).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("routes /intents to the bounded Intent Stack instead of the model", async () => {
    const user = userEvent.setup();
    const { onOpenIntentStack, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "/intents");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenIntentStack).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("routes /config to the desktop settings page", async () => {
    const user = userEvent.setup();
    const { onOpenSettings, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "/config");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it.each(["/doctor", "/feedback"])("routes %s to support and diagnostics", async (command) => {
    const user = userEvent.setup();
    const { onOpenSupport, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, command);
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onOpenSupport).toHaveBeenCalledOnce();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows unavailable agents without dispatching a run", async () => {
    const user = userEvent.setup();
    const { onSubmit, onNotice } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "@dis");
    await user.click(screen.getByRole("option", { name: /disabled/ }));

    expect(onNotice).toHaveBeenCalledWith(
      "Desktop agent execution requires the supervised child-session owner.",
      true,
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("queues Enter submissions by default while a run is active", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "Run this after the current task");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith("Run this after the current task", undefined, undefined);
    await waitFor(() => expect(editorText(input)).toBe(""));
  });

  it("keeps the durable queue available while live controls are recovering", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true, submissionBlocked: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "Keep working after the current task");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledWith(
      "Keep working after the current task",
      undefined,
      undefined,
    );
    await waitFor(() => expect(editorText(input)).toBe(""));
  });

  it("retains the draft when queue admission fails", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn(async () => false);
    renderComposer({
      active: true,
      submissionBlocked: true,
      onSubmit,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "Do not lose this draft");
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledOnce();
    expect(editorText(input)).toBe("Do not lose this draft");
  });

  it("does not queue an IME composition Enter", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "继续执行");
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });

    expect(onSubmit).not.toHaveBeenCalled();
    expect(editorText(input)).toBe("继续执行");
  });

  it("opens the durable queue without changing the draft", async () => {
    const user = userEvent.setup();
    const { onOpenQueue } = renderComposer({
      active: true,
      queueCount: 2,
      queuePanel: <span>Queued prompt controls</span>,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });

    typeEditorText(input, "Keep this draft");
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 2 messages" }));

    expect(onOpenQueue).toHaveBeenCalledOnce();
    expect(screen.getByRole("dialog", { name: "Open follow-up queue, 2 messages" })).toBeTruthy();
    expect(screen.getByText("Queued prompt controls")).toBeTruthy();
    expect(editorText(input)).toBe("Keep this draft");
  });

  it.each([
    { active: true, accepted: true }, { active: true, accepted: false },
    { active: false, accepted: true }, { active: false, accepted: false },
  ])("isolates a portal queue edit from the main draft (active=$active, accepted=$accepted)", async ({ active, accepted }) => {
    const user = userEvent.setup();
    let finishCommand: ((accepted: boolean) => void) | undefined;
    const onCommand = vi.fn((_action: ConversationQueueCommandAction) => new Promise<boolean>((resolve) => { finishCommand = resolve; }));
    const onSubmit = vi.fn(async () => false);
    const queue: ConversationQueueView = {
      schemaVersion: 1, sessionId: "queue-portal-session", generation: "queue-v1:before-edit",
      paused: false, totalItems: 2, truncated: false,
      items: ["first", "second"].map((entryId, order) => ({ entryId, order, kind: "chat", status: "queued",
        promptPreview: `${entryId} queued prompt`, promptPreviewTruncated: false, promptMaterial: "persisted_safe",
        dispatchable: false, blockedReason: "foreground_run_active" })),
    };
    renderComposer({ active, reviewActive: active, queueCount: 2, onSubmit,
      queuePanel: <ConversationQueuePanel queue={queue} busy={false} error={false}
        onRefresh={() => undefined} onCommand={onCommand} />,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const editor = editorView(input);
    const draft = "  # Preserve the independent main draft\n\n" + "- [ ] 保留中文与未知语法 {{raw}}。  \n".repeat(120) + "\nTail  ";
    setEditorText(input, draft);
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 2 messages" }));
    await user.click(screen.getAllByRole("button", { name: "Replace queued message" })[0]!);
    const replacement = screen.getByRole("textbox", { name: "Replacement prompt" }) as HTMLTextAreaElement;
    expect(input.closest("form")!.contains(replacement.closest("form"))).toBe(false);
    const edited = "  Edited queued prompt\nKeep its own whitespace.  \n";
    fireEvent.change(replacement, { target: { value: edited } });
    await user.click(screen.getByRole("button", { name: "Replace" }));
    expect(onCommand).toHaveBeenCalledExactlyOnceWith({ action: "edit", entryId: "first", prompt: edited, reasoningEffort: undefined });
    expect(onSubmit).not.toHaveBeenCalled();
    await act(async () => finishCommand?.(accepted));
    if (accepted) {
      await waitFor(() => expect(screen.queryByRole("textbox", { name: "Replacement prompt" })).toBeNull());
    } else {
      expect(screen.getByRole("textbox", { name: "Replacement prompt" })).toBe(replacement);
      expect(replacement.value).toBe(edited);
    }
    expect(onSubmit).not.toHaveBeenCalled();
    expect(editorText(input)).toBe(draft);
    expect(editorView(input)).toBe(editor);
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 2 messages" }));
    await user.click(screen.getByRole("button", { name: active ? "Queue message" : "Send message" }));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(draft, undefined, undefined);
  });

  it("handles only its own form submit when another portal form does not stop propagation", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true, queueCount: 1,
      queuePanel: <form aria-label="Independent portal form" onSubmit={(event) => event.preventDefault()}><button type="submit">Save independent form</button></form>,
    });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "Preserve this draft for an explicit main submit");
    await user.click(screen.getByRole("button", { name: "Open follow-up queue, 1 messages" }));
    await user.click(screen.getByRole("button", { name: "Save independent form" }));
    expect(onSubmit).not.toHaveBeenCalled();
    expect(editorText(input)).toBe("Preserve this draft for an explicit main submit");
    fireEvent.submit(input.closest("form")!);
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith("Preserve this draft for an explicit main submit", undefined, undefined);
  });
});

const draftImage: DraftImage = {
  attachmentId: "admitted-image-1", mimeType: "image/png", width: 2, height: 3,
  byteLen: 72, previewDataUrl: "data:image/png;base64,aW1hZ2U=",
};

describe("image attachments", () => {
  it("keeps images when a native command suggestion cannot carry them", async () => {
    const release = vi.fn(async () => undefined);
    const { onCompact } = renderComposer({ onPickImage: async () => draftImage, onReleaseImages: release });
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Attach image" }));
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    typeEditorText(input, "/comp");
    await user.click(screen.getByRole("option", { name: /Compact context/ }));
    expect(onCompact).not.toHaveBeenCalled();
    expect(screen.getByRole("img", { name: "Image 1" })).toBeTruthy();
    expect(release).not.toHaveBeenCalled();
  });

  it("retains an image and exact inline skill binding after rejection, then submits both", async () => {
    const submit = vi.fn().mockResolvedValueOnce(false).mockResolvedValue(true);
    const release = vi.fn(async () => undefined);
    renderComposer({ onPickImage: async () => draftImage, onSubmit: submit, onReleaseImages: release });
    const user = userEvent.setup();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    typeEditorText(input, "$rev");
    await user.click(screen.getByRole("option", { name: /Review/ }));
    await user.click(screen.getByRole("button", { name: "Attach image" }));
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(submit).toHaveBeenLastCalledWith("", {
      skillId: "review", skillSha256: "skill-sha", indexFingerprint: "index-sha",
    }, undefined, [draftImage]);
    expect(screen.getByText("$review")).toBeTruthy();
    expect(screen.getByRole("img", { name: "Image 1" })).toBeTruthy();
    expect(release).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(submit).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("img")).toBeNull();
    expect(release).toHaveBeenCalledWith([draftImage.attachmentId]);
  });

  it("sends image-only input without manufacturing prompt text", async () => {
    const submit = vi.fn(async () => true);
    renderComposer({ onPickImage: async () => draftImage, onSubmit: submit });
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.setup().click(screen.getByRole("button", { name: "Attach image" }));
    await userEvent.setup().click(screen.getByRole("button", { name: "Send message" }));
    expect(submit).toHaveBeenCalledWith("", undefined, undefined, [draftImage]);
  });

  it("previews a native selection and releases it on explicit removal", async () => {
    const release = vi.fn(async () => undefined);
    renderComposer({ onPickImage: async () => draftImage, onReleaseImages: release });
    await userEvent.setup().click(screen.getByRole("button", { name: "Attach image" }));
    expect(screen.getByRole("img", { name: "Image 1" }).getAttribute("src")).toBe(draftImage.previewDataUrl);
    await userEvent.setup().click(screen.getByRole("button", { name: "Remove image 1" }));
    expect(screen.queryByRole("img")).toBeNull();
    expect(release).toHaveBeenCalledWith([draftImage.attachmentId]);
  });

  it("pastes encoded bytes through the image ingress without interpreting a file path", async () => {
    const ingest = vi.fn(async () => draftImage);
    renderComposer({ onIngestImage: ingest });
    const file = new File([new Uint8Array([1, 2, 3])], "private-source-name.png", { type: "image/png" });
    Object.defineProperty(file, "arrayBuffer", { value: async () => new Uint8Array([1, 2, 3]).buffer });
    fireEvent.paste(screen.getByRole("combobox", { name: "Message Sigil" }), { clipboardData: { files: [file] } });
    await waitFor(() => expect(ingest).toHaveBeenCalledWith([1, 2, 3]));
    expect(await screen.findByRole("img", { name: "Image 1" })).toBeTruthy();
    expect(screen.queryByText("private-source-name.png")).toBeNull();
  });

  it("retains text and preview through a pending or rejected send, then releases an accepted draft", async () => {
    let settle!: (accepted: boolean) => void;
    const pending = new Promise<boolean>((resolve) => { settle = resolve; });
    const submit = vi.fn().mockReturnValueOnce(pending).mockResolvedValue(true);
    const release = vi.fn(async () => undefined);
    renderComposer({ onPickImage: async () => draftImage, onSubmit: submit, onReleaseImages: release });
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Attach image" }));
    const input = screen.getByRole("combobox", { name: "Message Sigil" }) as HTMLElement;
    typeEditorText(input, "Describe this diagram");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(submit).toHaveBeenCalledWith("Describe this diagram", undefined, undefined, [draftImage]);
    expect(editorText(input)).toBe("Describe this diagram");
    expect(screen.getByRole("img", { name: "Image 1" })).toBeTruthy();
    await act(async () => { settle(false); await pending; });
    expect(editorText(input)).toBe("Describe this diagram");
    expect(release).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(editorText(input)).toBe("");
    expect(screen.queryByRole("img")).toBeNull();
    expect(release).toHaveBeenCalledWith([draftImage.attachmentId]);
  });

  it("releases a native selection that completes after its composer unmounts", async () => {
    let settle!: (image: DraftImage) => void;
    const pending = new Promise<DraftImage>((resolve) => { settle = resolve; });
    const release = vi.fn(async () => undefined);
    renderComposer({ onPickImage: () => pending, onReleaseImages: release });
    await userEvent.setup().click(screen.getByRole("button", { name: "Attach image" }));
    cleanup();
    await act(async () => { settle(draftImage); await pending; });
    expect(release).toHaveBeenCalledWith([draftImage.attachmentId]);
  });
});

it.each([true, false])("settles the current draft key when durable identity arrives during a pending send (%s)", async (accepted) => {
  let settle: ((value: boolean) => void) | undefined;
  const submit = vi.fn(() => new Promise<boolean>((resolve) => { settle = resolve; }));
  const view = renderComposer({ onSubmit: submit });
  const input = screen.getByRole("combobox", { name: "Message Sigil" }) as HTMLElement;
  const draft = "保留中文草稿，金额 198.01。\n参与者：小林|小周。";
  setEditorText(input, draft);
  await userEvent.click(screen.getByRole("button", { name: "Send message" }));
  await waitFor(() => expect(submit).toHaveBeenCalledOnce());
  act(() => view.bindPersistentDraftKey("stable-durable-draft"));
  expect(screen.getByRole("combobox", { name: "Message Sigil" })).toBe(input);
  expect(window.localStorage.getItem("stable-durable-draft")).toBe(draft);
  await act(async () => settle?.(accepted));
  await waitFor(() => expect(editorText(input)).toBe(accepted ? "" : draft));
  expect(window.localStorage.getItem("stable-durable-draft")).toBe(accepted ? null : draft);
  view.unmount();
  renderComposer({ persistentDraftKey: "stable-durable-draft", onSubmit: submit });
  expect(editorText(screen.getByRole("combobox", { name: "Message Sigil" }))).toBe(accepted ? "" : draft);
  expect(submit).toHaveBeenCalledOnce();
});

describe("Markdown writing", () => {
  it("restores the preview reading offset after hiding it independently of source scrolling and late draft persistence", async () => {
    const user = userEvent.setup();
    const { bindPersistentDraftKey } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = "# START\n\n" + Array.from({ length: 60 }, (_, index) =>
      `- [ ] 第${index + 1}项：**重点**，继续阅读 [参考](https://example.test) 与 \`code\`。\n`
    ).join("\n") + "\nEND\n";
    setEditorText(input, source);
    const view = editorView(input);
    act(() => view.dispatch({ selection: { anchor: source.length - 10, head: source.length } }));
    const selection = view.state.selection;
    view.scrollDOM.scrollTop = 210;
    await user.click(screen.getByRole("button", { name: "Preview" }));
    let preview = screen.getByRole("region", { name: "Preview" });
    // JSDOM records offsets; native verification owns actual wrapped line geometry.
    fireEvent.scroll(preview, { target: { scrollTop: 1320 } });
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.queryByRole("region", { name: "Preview" })).toBeNull();
    bindPersistentDraftKey("late-durable-reading-draft");
    await user.click(screen.getByRole("button", { name: "Preview" }));
    preview = screen.getByRole("region", { name: "Preview" });
    expect(preview.scrollTop).toBe(1320);
    expect(view.scrollDOM.scrollTop).toBe(210);
    expect(editorView(input)).toBe(view);
    expect(view.state.selection.eq(selection)).toBe(true);
    expect(editorText(input)).toBe(source);
  });

  it("preserves independent preview reading across expanded editing and an arriving review", async () => {
    const user = userEvent.setup();
    const { bindReviewActive } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "## Draft\n\nReading paragraph\n\n".repeat(60));
    const view = editorView(input);
    view.scrollDOM.scrollTop = 90;
    await user.click(screen.getByRole("button", { name: "Preview" }));
    const preview = screen.getByRole("region", { name: "Preview" });
    fireEvent.scroll(preview, { target: { scrollTop: 960 } });
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    expect(preview.scrollTop).toBe(960);
    fireEvent.scroll(preview, { target: { scrollTop: 1240 } });
    bindReviewActive(true);
    expect(preview.scrollTop).toBe(1240);
    bindReviewActive(false);
    await user.click(screen.getByRole("button", { name: "Collapse editor" }));
    expect(preview.scrollTop).toBe(1240);
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.getByRole("region", { name: "Preview" }).scrollTop).toBe(1240);
    expect(view.scrollDOM.scrollTop).toBe(90);
    expect(editorView(input)).toBe(view);
  });

  it.each([true, false])("%s accepted submission resets only the cleared draft's preview reading position", async (accepted) => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ onSubmit: vi.fn(async () => accepted) });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = "A long draft paragraph\n\n".repeat(60);
    setEditorText(input, source);
    await user.click(screen.getByRole("button", { name: "Preview" }));
    fireEvent.scroll(screen.getByRole("region", { name: "Preview" }), { target: { scrollTop: 900 } });
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onSubmit).toHaveBeenCalledOnce();
    await waitFor(() => expect(editorText(input)).toBe(accepted ? "" : source));
    if (accepted) setEditorText(input, "A different new draft\n\n".repeat(60));
    expect(screen.getByRole("region", { name: "Preview" }).scrollTop).toBe(accepted ? 0 : 900);
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.getByRole("region", { name: "Preview" }).scrollTop).toBe(accepted ? 0 : 900);
  });

  it("does not transfer preview reading to another mounted draft", async () => {
    const user = userEvent.setup();
    const first = renderComposer({ draftKey: "workspace:first-session", persistentDraftKey: "first-reading-draft" });
    setEditorText(screen.getByRole("combobox", { name: "Message Sigil" }), "First draft\n\n".repeat(60));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    fireEvent.scroll(screen.getByRole("region", { name: "Preview" }), { target: { scrollTop: 1200 } });
    first.unmount();
    renderComposer({ draftKey: "workspace:second-session", persistentDraftKey: "second-reading-draft" });
    setEditorText(screen.getByRole("combobox", { name: "Message Sigil" }), "Second draft\n\n".repeat(60));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.getByRole("region", { name: "Preview" }).scrollTop).toBe(0);
  });

  it("retains preview reading for later edits when an older submission is accepted", async () => {
    const user = userEvent.setup();
    let settle!: (accepted: boolean) => void;
    const pending = new Promise<boolean>((resolve) => { settle = resolve; });
    renderComposer({ onSubmit: vi.fn(() => pending) });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "Submitted draft\n\n".repeat(60));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Send message" }));
    const laterDraft = "Later unsent edits\n\n".repeat(60);
    setEditorText(input, laterDraft);
    fireEvent.scroll(screen.getByRole("region", { name: "Preview" }), { target: { scrollTop: 750 } });
    await act(async () => { settle(true); await pending; });
    expect(editorText(input)).toBe(laterDraft);
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.getByRole("region", { name: "Preview" }).scrollTop).toBe(750);
  });

  it("edits source and renders a live safe preview without remounting the editor or losing history", async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "中文 *emphasis*");
    const view = editorView(input);
    act(() => view.dispatch({ selection: { anchor: 0, head: 2 } }));
    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("button", { name: /Bold/ }));
    expect(editorText(input)).toBe("**中文** *emphasis*");
    expect(document.activeElement).toBe(input);
    const preview = screen.getByRole("region", { name: "Preview" });
    expect(preview.querySelector("strong")?.textContent).toBe("中文");
    expect(preview.querySelector("em")?.textContent).toBe("emphasis");
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(editorView(screen.getByRole("combobox", { name: "Message Sigil" }))).toBe(view);
    input.focus();
    fireEvent.keyDown(input, { key: "z", code: "KeyZ", ctrlKey: true });
    expect(editorText(input)).toBe("中文 *emphasis*");
    await waitFor(() => expect(screen.getByRole("region", { name: "Preview" }).querySelector("strong")).toBeNull());
    fireEvent.keyDown(input, { key: "y", code: "KeyY", ctrlKey: true });
    expect(editorText(input)).toBe("**中文** *emphasis*");
  });

  it("retains untrusted Markdown source while the preview cannot create active HTML or remote images", async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = '<script>alert(1)</script>\n<img src="https://remote.invalid/pixel" onerror="alert(2)">\n![remote](https://remote.invalid/image.png)\n[bad](javascript:alert(3))\n\n**safe**';
    setEditorText(input, source);
    await user.click(screen.getByRole("button", { name: "Preview" }));
    const preview = screen.getByRole("region", { name: "Preview" });
    expect(preview.querySelector("script,img,iframe,object,[onerror]" )).toBeNull();
    expect(preview.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(preview.querySelector("strong")?.textContent).toBe("safe");
    expect(editorText(input)).toBe(source);
  });

  it("sends the exact Markdown source, including whitespace and code fences, from expanded editing", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = "  中文🙂  \r\n```sh\nprintf 'hello'\n```\n  ";
    setEditorText(input, source);
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    expect(screen.getByText("Enter to send · Shift+Enter for a new line")).toBeTruthy();
    fireEvent.keyDown(input, { key: "Enter", code: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith(source, undefined, undefined);
  });

  it("guards composition before an open suggestion can consume Enter or Tab", () => {
    const { onOpenSettings, onSubmit } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "/config");
    expect(screen.getByRole("listbox", { name: "Composer suggestions" })).toBeTruthy();
    fireEvent.compositionStart(input);
    fireEvent.keyDown(input, { key: "Enter", code: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Tab", code: "Tab", isComposing: true });
    expect(onOpenSettings).not.toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
    expect(editorText(input)).toBe("/config");
    fireEvent.compositionEnd(input);
  });

  it("does not clear an accepted submission when a new composition has started", async () => {
    let settle!: (accepted: boolean) => void;
    const pending = new Promise<boolean>((resolve) => { settle = resolve; });
    const { onSubmit } = renderComposer({ onSubmit: vi.fn(() => pending) });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "submitted draft");
    fireEvent.submit(input.closest("form")!);
    expect(onSubmit).toHaveBeenCalledOnce();
    fireEvent.compositionStart(input);
    setEditorText(input, "submitted draft新的中文");
    await act(async () => { settle(true); await pending; });
    expect(editorText(input)).toBe("submitted draft新的中文");
    fireEvent.compositionEnd(input);
  });

  it("keeps the current approval space bounded even when source and preview are expanded", async () => {
    const user = userEvent.setup();
    renderComposer({ active: true, reviewActive: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "A long follow-up\n".repeat(30));
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(editorView(input).scrollDOM.style.maxHeight).toBe("88px");
    expect(input.closest(".composer-document")?.getAttribute("data-review-active")).toBe("true");
    expect(screen.getByRole("region", { name: "Preview" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel work" })).toBeTruthy();
  });

  it("keeps the last persisted draft when a larger edit exceeds the storage budget", () => {
    window.localStorage.setItem("composer-test", "saved draft");
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const large = "中".repeat(90_000);
    setEditorText(input, large);
    expect(editorText(input)).toBe(large);
    expect(window.localStorage.getItem("composer-test")).toBe("saved draft");
    expect(screen.getByText(/These edits are only in this window/)).toBeTruthy();
  });

  it("retains the previous storage key when late durable identity cannot be persisted", () => {
    window.localStorage.setItem("composer-test", "saved draft");
    const { bindPersistentDraftKey } = renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const setItem = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => { throw new DOMException("full", "QuotaExceededError"); });
    bindPersistentDraftKey("durable-key");
    expect(screen.getByRole("combobox", { name: "Message Sigil" })).toBe(input);
    expect(editorText(input)).toBe("saved draft");
    expect(window.localStorage.getItem("composer-test")).toBe("saved draft");
    expect(window.localStorage.getItem("durable-key")).toBeNull();
    expect(screen.getByText(/These edits are only in this window/)).toBeTruthy();
    setItem.mockRestore();
  });
});


it("uses Shift+Tab for backward focus without accepting a suggestion, while Tab still accepts it", async () => {
  const user = userEvent.setup();
  const { onOpenSettings, onSubmit } = renderComposer();
  const input = screen.getByRole("combobox", { name: "Message Sigil" });
  typeEditorText(input, "/config");
  expect(screen.getByRole("listbox", { name: "Composer suggestions" })).toBeTruthy();
  await user.tab({ shift: true });
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Expand editor" }));
  expect(editorText(input)).toBe("/config");
  expect(onOpenSettings).not.toHaveBeenCalled();
  expect(onSubmit).not.toHaveBeenCalled();
  input.focus();
  await user.tab();
  await waitFor(() => expect(onOpenSettings).toHaveBeenCalledOnce());
  expect(onSubmit).not.toHaveBeenCalled();
});


function loadConversationStyles(): HTMLStyleElement {
  const parsed = document.createElement("style");
  parsed.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
  document.head.append(parsed);
  const features = Array.from(parsed.sheet!.cssRules).find((rule) =>
    "name" in rule && rule.name === "features"
  ) as CSSGroupingRule;
  const style = document.createElement("style");
  style.textContent = Array.from(features.cssRules, (rule) => rule.cssText).join("\n");
  parsed.replaceWith(style);
  return style;
}

describe("expanded composer", () => {
  it("keeps history recovery, gap notices and terminal controls reachable beside an empty placeholder", async () => {
    const style = loadConversationStyles();
    try {
      const user = userEvent.setup();
      const retry = vi.fn();
      const stop = vi.fn();
      const { container } = renderComposer({ conversationHistory: "empty", timelineSupplement: <>
        <div role="status">Live history has a gap</div>
        <button type="button" onClick={retry}>Retry saved messages</button>
        <article aria-label="Running terminal"><button type="button" onClick={stop}>Stop terminal</button></article>
      </> });
      await user.click(screen.getByRole("button", { name: "Expand editor" }));
      expect(getComputedStyle(container.querySelector(".timeline")!).display).not.toBe("none");
      await user.click(screen.getByRole("button", { name: "Retry saved messages" }));
      await user.click(screen.getByRole("button", { name: "Stop terminal" }));
      expect(retry).toHaveBeenCalledOnce();
      expect(stop).toHaveBeenCalledOnce();
      expect(screen.getByRole("status").textContent).toBe("Live history has a gap");
    } finally { style.remove(); }
  });

  it("anchors expanded suggestions inside the writing budget and selects the real command without submitting", async () => {
    const style = loadConversationStyles();
    try {
      const user = userEvent.setup();
      const { onSubmit, onOpenSettings } = renderComposer({ conversationHistory: "empty" });
      const input = screen.getByRole("combobox", { name: "Message Sigil" });
      const view = editorView(input);
      const documentArea = input.closest(".composer-document")!;
      setEditorText(input, "/config");
      expect(documentArea.contains(screen.getByRole("listbox", { name: "Composer suggestions" }))).toBe(false);
      await user.click(screen.getByRole("button", { name: "Expand editor" }));
      const list = screen.getByRole("listbox", { name: "Composer suggestions" });
      expect(documentArea.contains(list)).toBe(true);
      expect(screen.getAllByRole("listbox", { name: "Composer suggestions" })).toHaveLength(1);
      expect(getComputedStyle(documentArea).position).toBe("relative");
      expect(getComputedStyle(list).bottom).toBe("auto");
      await user.click(within(list).getByRole("option", { name: /\/config/ }));
      expect(onOpenSettings).toHaveBeenCalledOnce();
      expect(onSubmit).not.toHaveBeenCalled();
      expect(editorView(input)).toBe(view);
      expect(screen.getByRole("button", { name: "Collapse editor" })).toBeTruthy();
    } finally { style.remove(); }
  });

  it.each(["empty", "populated"] as const)("allocates remaining conversation space to writing with %s history and restores the normal layout", async (history) => {
    const style = loadConversationStyles();
    try {
      const user = userEvent.setup();
      const { onSubmit, container } = renderComposer({ conversationHistory: history });
      const input = screen.getByRole("combobox", { name: "Message Sigil" });
      const source = "# Long draft\n\n    exact source  \n".repeat(50);
      setEditorText(input, source);
      const view = editorView(input);
      const form = input.closest("form")!;
      const timeline = container.querySelector<HTMLElement>(".timeline")!;
      expect(Number(getComputedStyle(form).flexGrow)).toBe(0);
      await user.click(screen.getByRole("button", { name: "Preview" }));
      await user.click(screen.getByRole("button", { name: "Expand editor" }));
      const documentArea = input.closest(".composer-document")!;
      const preview = screen.getByRole("region", { name: "Preview" });
      // jsdom has no layout engine: qualify the mounted flex/scroll contract here,
      // then measure the resulting short-window geometry in the native WebView.
      expect(Number(getComputedStyle(form).flexGrow)).toBeGreaterThan(Number(getComputedStyle(timeline).flexGrow));
      expect(Number(getComputedStyle(documentArea).flexGrow)).toBeGreaterThan(0);
      expect(view.scrollDOM.style.maxHeight).toBe("100%");
      expect(getComputedStyle(preview).overflow).toBe("auto");
      expect(getComputedStyle(preview).maxHeight).toBe("100%");
      if (history === "empty") expect(getComputedStyle(timeline).display).toBe("none");
      else expect(parseFloat(getComputedStyle(timeline).flexBasis)).toBeGreaterThan(0);
      expect(screen.getByRole("button", { name: "Send message" })).toBeTruthy();
      await user.click(screen.getByRole("button", { name: "Collapse editor" }));
      expect(Number(getComputedStyle(form).flexGrow)).toBe(0);
      expect(Number(getComputedStyle(timeline).flexGrow)).toBeGreaterThan(0);
      expect(getComputedStyle(timeline).display).not.toBe("none");
      expect(editorView(input)).toBe(view);
      expect(editorText(input)).toBe(source);
      expect(onSubmit).not.toHaveBeenCalled();
    } finally { style.remove(); }
  });

  it.each(["source", "preview", "editing tools", "send tools"] as const)("collapses from %s focus across a viewport resize without changing the editor, selection, history or running work", async (target) => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    const { onSubmit } = renderComposer({ active: true, onCancel });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = "  # Exact draft\r\n\n```text\n  keep whitespace  \n```\n";
    setEditorText(input, source);
    const view = editorView(input);
    act(() => view.dispatch({ selection: { anchor: 4, head: 12 } }));
    const selection = view.state.selection;
    await user.click(screen.getByRole("button", { name: "Preview" }));
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    const focusTarget = target === "source" ? input : target === "preview"
      ? screen.getByRole("region", { name: "Preview" }) : target === "editing tools"
        ? screen.getByRole("button", { name: "Preview" }) : screen.getByRole("button", { name: "Queue message" });
    focusTarget.focus();
    const previousHeight = window.innerHeight;
    try {
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 560 });
      fireEvent.resize(window);
      fireEvent.keyDown(focusTarget, { key: "Escape", code: "Escape" });
      expect(screen.getByRole("button", { name: "Expand editor" })).toBeTruthy();
      expect(document.activeElement).toBe(input);
      expect(editorView(input)).toBe(view);
      expect(view.state.selection.eq(selection)).toBe(true);
      expect(editorText(input)).toBe(source);
      fireEvent.keyDown(input, { key: "z", code: "KeyZ", ctrlKey: true });
      expect(editorText(input)).toBe("");
      fireEvent.keyDown(input, { key: "y", code: "KeyY", ctrlKey: true });
      expect(editorText(input)).toBe(source);
      expect(onSubmit).not.toHaveBeenCalled();
      expect(onCancel).not.toHaveBeenCalled();
    } finally { Object.defineProperty(window, "innerHeight", { configurable: true, value: previousHeight }); }
  });

  it("returns space to an arriving approval and restores expanded writing when review ends without remounting", async () => {
    const style = loadConversationStyles();
    try {
      const user = userEvent.setup();
      const { bindReviewActive, container } = renderComposer({ active: true, conversationHistory: "empty" });
      const input = screen.getByRole("combobox", { name: "Message Sigil" });
      setEditorText(input, "Approval follow-up\n".repeat(30));
      const view = editorView(input);
      const form = input.closest("form")!;
      const timeline = container.querySelector<HTMLElement>(".timeline")!;
      await user.click(screen.getByRole("button", { name: "Expand editor" }));
      await user.click(screen.getByRole("button", { name: "Preview" }));
      bindReviewActive(true);
      expect(Number(getComputedStyle(form).flexGrow)).toBe(0);
      expect(getComputedStyle(timeline).display).not.toBe("none");
      expect(view.scrollDOM.style.maxHeight).toBe("88px");
      expect(getComputedStyle(screen.getByRole("region", { name: "Preview" })).maxHeight).toBe("88px");
      expect(screen.getByRole("button", { name: "Cancel work" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Queue message" })).toBeTruthy();
      bindReviewActive(false);
      expect(Number(getComputedStyle(form).flexGrow)).toBeGreaterThan(0);
      expect(getComputedStyle(timeline).display).toBe("none");
      expect(view.scrollDOM.style.maxHeight).toBe("100%");
      expect(editorView(input)).toBe(view);
      expect(editorText(input)).toBe("Approval follow-up\n".repeat(30));
    } finally { style.remove(); }
  });

  it("dismisses suggestions before collapsing from source or toolbar focus", async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    for (const fromToolbar of [false, true]) {
      await user.click(screen.getByRole("button", { name: "Expand editor" }));
      setEditorText(input, fromToolbar ? "/doctor" : "/config");
      const target = fromToolbar ? screen.getByRole("button", { name: "Preview" }) : input;
      target.focus();
      fireEvent.keyDown(target, { key: "Escape", code: "Escape" });
      expect(screen.queryByRole("listbox", { name: "Composer suggestions" })).toBeNull();
      expect(screen.getByRole("button", { name: "Collapse editor" })).toBeTruthy();
      fireEvent.keyDown(target, { key: "Escape", code: "Escape" });
      expect(screen.getByRole("button", { name: "Expand editor" })).toBeTruthy();
    }
  });

  it("lets the format popover close first and then collapses from its restored trigger focus", async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "draft");
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    await user.click(screen.getByRole("button", { name: "Format" }));
    const format = screen.getByRole("button", { name: "Format" });
    const bold = screen.getByRole("button", { name: /^Bold/ });
    bold.focus();
    fireEvent.keyDown(bold, { key: "Escape", code: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Format" })).toBeNull();
    expect(document.activeElement).toBe(format);
    expect(screen.getByRole("button", { name: "Collapse editor" })).toBeTruthy();
    fireEvent.keyDown(format, { key: "Escape", code: "Escape" });
    expect(screen.getByRole("button", { name: "Expand editor" })).toBeTruthy();
    expect(document.activeElement).toBe(input);
  });

  it("leaves IME composition and an already collapsed composer alone", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    const { onSubmit } = renderComposer({ active: true, onCancel });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "中文草稿");
    fireEvent.keyDown(input, { key: "Escape", code: "Escape" });
    expect(screen.getByRole("button", { name: "Expand editor" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Expand editor" }));
    fireEvent.compositionStart(input);
    fireEvent.keyDown(input, { key: "Escape", code: "Escape", isComposing: true, keyCode: 229 });
    expect(screen.getByRole("button", { name: "Collapse editor" })).toBeTruthy();
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Escape", code: "Escape" });
    expect(screen.getByRole("button", { name: "Expand editor" })).toBeTruthy();
    expect(editorText(input)).toBe("中文草稿");
    expect(onSubmit).not.toHaveBeenCalled();
    expect(onCancel).not.toHaveBeenCalled();
  });
});

describe("compact composer presentation", () => {
  it("keeps the image picker in the editing toolbar with accessible busy feedback and the exact draft", async () => {
    const user = userEvent.setup();
    let settle!: (image: DraftImage | null) => void;
    const pending = new Promise<DraftImage | null>((resolve) => { settle = resolve; });
    const onPickImage = vi.fn(() => pending);
    const { onSubmit } = renderComposer({ onPickImage });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    const source = "    原始草稿  \n";
    setEditorText(input, source);
    const attach = screen.getByRole("button", { name: "Attach image" });
    const tools = attach.closest(".composer-editing-tools");
    expect(tools?.contains(screen.getByRole("button", { name: "Format" }))).toBe(true);
    expect(tools?.contains(screen.getByRole("button", { name: "Preview" }))).toBe(true);
    expect(attach.getAttribute("title")).toBe("Attach image");
    expect(attach.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
    expect(attach.textContent).toBe("");
    await user.click(attach);
    expect(onPickImage).toHaveBeenCalledOnce();
    const busy = screen.getByRole("button", { name: "Attaching image…" });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    expect(screen.getByRole("combobox", { name: "Message Sigil" })).toBe(input);
    expect(editorText(input)).toBe(source);
    await act(async () => { settle(draftImage); await pending; });
    expect(screen.getByRole("img", { name: "Image 1" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Attach image" }) as HTMLButtonElement).disabled).toBe(false);
    expect(editorText(input)).toBe(source);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("uses controlled Markdown typography for preview headings, task lists and quotes without changing source", async () => {
    const parsed = document.createElement("style");
    parsed.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
    document.head.append(parsed);
    const features = Array.from(parsed.sheet!.cssRules).find((rule) =>
      "name" in rule && rule.name === "features"
    ) as CSSGroupingRule;
    const style = document.createElement("style");
    style.textContent = Array.from(features.cssRules, (rule) => rule.cssText).join("\n");
    parsed.replaceWith(style);
    try {
      const user = userEvent.setup();
      renderComposer();
      const input = screen.getByRole("combobox", { name: "Message Sigil" });
      const source = "# Preview heading\n\nA paragraph.\n\n> A quote.\n\n- [ ] First **重点** [资料](https://example.com/notes) 与 `task-code` 尾部\n  - [x] Nested **完整**\n- [x] Second\n\n`inline`\n\n```text\n  exact code  \n```";
      setEditorText(input, source);
      const editor = editorView(input);
      await user.click(screen.getByRole("button", { name: "Preview" }));
      const preview = screen.getByRole("region", { name: "Preview" });
      expect(getComputedStyle(within(preview).getByRole("heading", { level: 1 })).fontSize).toBe("19px");
      expect(getComputedStyle(preview.querySelector(".contains-task-list")!).listStyle).toBe("none");
      expect(getComputedStyle(preview.querySelector(".task-list-item")!).display).toBe("list-item");
      const task = preview.querySelector(".task-list-item")!;
      expect(task.querySelector("strong")?.textContent).toBe("重点");
      expect(task.querySelector("a")?.textContent).toBe("资料");
      expect(task.querySelector("code")?.textContent).toBe("task-code");
      expect(task.querySelector(":scope > ul > .task-list-item strong")?.textContent).toBe("完整");
      const checkbox = task.querySelector("input")! as HTMLInputElement;
      expect(checkbox.disabled).toBe(true);
      expect(checkbox.getAttribute("aria-label")).toBe("Incomplete task");
      expect(getComputedStyle(preview.querySelector("blockquote")!).paddingLeft).toBe("10px");
      expect(getComputedStyle(preview.querySelector("blockquote")!).marginLeft).toBe("0px");
      expect(preview.querySelector("pre code")?.textContent).toBe("  exact code  \n");
      expect(getComputedStyle(preview.querySelector("pre code")!).padding).not.toBe("1px 4px");
      expect(editorText(input)).toBe(source);
      expect(editorView(input)).toBe(editor);
    } finally {
      style.remove();
    }
  });
});

describe("compact Markdown format presentation", () => {
  it.each([
    ["MacIntel", "⌘B", "Meta+B"],
    ["Win32", "Ctrl+B", "Control+B"],
    ["Linux x86_64", "Ctrl+B", "Control+B"],
  ])("shows only the %s modifier and keeps every format action without replacing the editor", async (platform, shortcut, accessibleShortcut) => {
    const platformGetter = vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
    try {
      const user = userEvent.setup();
      renderComposer();
      const input = screen.getByRole("combobox", { name: "Message Sigil" });
      setEditorText(input, "  中文 raw {{unknown}}  ");
      const editor = editorView(input);
      act(() => editor.dispatch({ selection: { anchor: 2, head: 4 } }));
      await user.click(screen.getByRole("button", { name: "Format" }));
      const menu = screen.getByRole("dialog", { name: "Format" });
      const actions = within(menu).getAllByRole("button");
      expect(actions.map((button) => button.textContent?.replace(/⌘[BIK]|Ctrl\+[BIK]/g, ""))).toEqual(["Bold", "Italic", "Inline code", "Code block", "List", "Link"]);
      const bold = within(menu).getByRole("button", { name: "Bold" });
      expect(bold.querySelector("kbd")?.textContent).toBe(shortcut);
      expect(bold.getAttribute("aria-keyshortcuts")).toBe(accessibleShortcut);
      expect(menu.textContent).not.toContain(platform === "MacIntel" ? "Ctrl" : "⌘");
      await user.click(bold);
      expect(editorView(input)).toBe(editor);
      expect(editorText(input)).toBe("  **中文** raw {{unknown}}  ");
      expect(document.activeElement).toBe(input);
    } finally {
      platformGetter.mockRestore();
    }
  });

  it("describes queueing after this response without changing the queued source", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderComposer({ active: true });
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    setEditorText(input, "  exact follow-up  ");
    const queueButton = screen.getByRole("button", { name: "Queue message" });
    await user.hover(queueButton);
    expect(await screen.findByRole("tooltip")).toHaveProperty("textContent", "Queue for after this response (Enter)");
    await user.click(queueButton);
    expect(onSubmit).toHaveBeenCalledWith("  exact follow-up  ", undefined, undefined);
  });
});
