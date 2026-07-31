import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from "react";

import { ComposerSuggestions, type ComposerSuggestion } from "./ComposerSuggestions";
import type { ComposerActivityState } from "./features/conversation/composerActivity";
import { modelOptionIsSelectable, providerModelRefKey, providerModelRefsEqual } from "./types";
import type {
  AgentBinding,
  AgentCatalogEntry,
  ProviderConnection,
  ProviderModelRef,
  PermissionMode,
  ReasoningEffort,
  ModelOption,
  RunContext,
  SkillBinding,
  SkillCatalogEntry,
} from "./types";
import { type Translate, useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { Button, Dialog, IconButton, Popover, Select, TextArea, Tooltip } from "./ui/primitives";

const MAX_DRAFT_BYTES = 256 * 1024;
const MAX_COMPOSER_HEIGHT = 176;

export function Composer({
  draftKey,
  active,
  submissionBlocked,
  queueSubmissionBlocked = false,
  draftEditingBlocked = false,
  submitting,
  controlBusy,
  composerRef,
  runContext,
  runContextBusy,
  providerConnections,
  selectedModelRef,
  permissionMode,
  reasoningEffort,
  requestedSkill,
  requestedAgent,
  queueCount,
  queuePaused,
  queueBusy,
  queuePanel,
  activityState,
  onModelChange,
  onPermissionModeChange,
  onReasoningEffortChange,
  onNewSession,
  onOpenSessionPicker,
  onOpenSettings,
  onOpenSupport,
  onOpenAgentWorkbench,
  onOpenQueue,
  onCompact,
  onOpenIntentStack,
  onNotice,
  onSubmit,
  onInterruptAndRunNext,
  onCancel,
}: {
  draftKey: string;
  active: boolean;
  submissionBlocked: boolean;
  queueSubmissionBlocked?: boolean;
  draftEditingBlocked?: boolean;
  submitting: boolean;
  controlBusy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  runContext?: RunContext;
  runContextBusy: boolean;
  providerConnections?: ProviderConnection[];
  selectedModelRef?: ProviderModelRef;
  permissionMode: PermissionMode;
  reasoningEffort?: ReasoningEffort;
  requestedSkill?: SkillCatalogEntry;
  requestedAgent?: AgentCatalogEntry;
  queueCount: number;
  queuePaused: boolean;
  queueBusy: boolean;
  queuePanel?: ReactNode;
  activityState?: ComposerActivityState;
  onModelChange: (modelRef: ProviderModelRef) => void;
  onPermissionModeChange: (mode: PermissionMode) => void;
  onReasoningEffortChange: (effort: ReasoningEffort) => void;
  onNewSession: () => Promise<boolean>;
  onOpenSessionPicker: (query: string) => void;
  onOpenSettings: () => void;
  onOpenSupport: () => void;
  onOpenAgentWorkbench: (query: string) => void;
  onOpenQueue: () => void;
  onCompact: () => Promise<boolean>;
  onOpenIntentStack: () => void;
  onNotice: (message: string, error?: boolean) => void;
  onSubmit: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding) => Promise<boolean>;
  onInterruptAndRunNext: (prompt: string) => Promise<boolean>;
  onCancel: () => void;
}) {
  const { t } = useLocale();
  const [prompt, setPrompt] = useState(() => readDraft(draftKey));
  const [selectedSkill, setSelectedSkill] = useState<SkillCatalogEntry>();
  const [selectedAgent, setSelectedAgent] = useState<AgentCatalogEntry>();
  const [activeSuggestion, setActiveSuggestion] = useState(0);
  const [suggestionsDismissedFor, setSuggestionsDismissedFor] = useState<string>();
  const [interruptPrompt, setInterruptPrompt] = useState<string>();
  const suggestionListId = `${useId()}-suggestions`;
  const modelSelectRef = useRef<HTMLSelectElement>(null);
  const effortSelectRef = useRef<HTMLSelectElement>(null);
  useEffect(() => {
    if (requestedSkill === undefined) return;
    setSelectedSkill(requestedSkill);
    setSelectedAgent(undefined);
    requestAnimationFrame(() => focusWithoutScroll(composerRef.current));
  }, [composerRef, requestedSkill]);
  useEffect(() => {
    if (requestedAgent === undefined) return;
    setSelectedAgent(requestedAgent);
    setSelectedSkill(undefined);
    requestAnimationFrame(() => focusWithoutScroll(composerRef.current));
  }, [composerRef, requestedAgent]);
  const suggestionQuery = leadingInvocationToken(prompt);
  const suggestions = useMemo(
    () => buildSuggestions(runContext, suggestionQuery, selectedSkill),
    [runContext, selectedSkill, suggestionQuery],
  );
  const suggestionsOpen =
    suggestionQuery !== undefined &&
    suggestions.length > 0 &&
    suggestionsDismissedFor !== suggestionQuery.token;
  useLayoutEffect(() => {
    const input = composerRef.current;
    if (input === null) return;
    input.style.height = "0px";
    const nextHeight = Math.min(input.scrollHeight, MAX_COMPOSER_HEIGHT);
    input.style.height = `${nextHeight}px`;
    input.style.overflowY = input.scrollHeight > MAX_COMPOSER_HEIGHT ? "auto" : "hidden";
  }, [composerRef, prompt]);

  const submit = async () => {
    let nextPrompt = prompt.trim();
    if (
      nextPrompt === ""
      || (active ? queueSubmissionBlocked : submissionBlocked)
      || submitting
      || (active && queueBusy)
    ) return;
    const command = resolveCommand(runContext, nextPrompt);
    if (command !== undefined) {
      if (await executeCommand(command.suggestion, command.argument)) clearComposer();
      return;
    }
    const directSkill = selectedSkill === undefined && selectedAgent === undefined
      ? resolveSkill(runContext, nextPrompt)
      : undefined;
    const skill = selectedSkill ?? directSkill?.skill;
    if (directSkill !== undefined) nextPrompt = directSkill.prompt;
    const directAgent = selectedAgent === undefined && skill === undefined
      ? resolveAgent(runContext, nextPrompt)
      : undefined;
    const agent = selectedAgent ?? directAgent?.agent;
    if (directAgent !== undefined) nextPrompt = directAgent.prompt;
    if (nextPrompt === "") {
      onNotice(t(agent === undefined ? "skillNeedsPrompt" : "agentNeedsPrompt"), true);
      return;
    }
    if (skill !== undefined && (!skill.available || skill.binding === undefined)) {
      onNotice(skill.unavailableReason ?? t("extensionUnavailable"), true);
      return;
    }
    if (nextPrompt.startsWith("@") && agent === undefined) {
      onNotice(t("agentExecutionUnavailable"), true);
      return;
    }
    if (agent !== undefined && (!agent.available || agent.binding === undefined)) {
      onNotice(agent.unavailableReason ?? t("agentExecutionUnavailable"), true);
      return;
    }
    if (active && (skill !== undefined || agent !== undefined)) {
      onNotice(t("queueExtensionBindingUnavailable"), true);
      return;
    }
    if (await onSubmit(nextPrompt, skill?.binding, agent?.binding)) {
      clearComposer();
    }
  };
  const requestInterruptAndRunNext = () => {
    const nextPrompt = prompt.trim();
    if (nextPrompt === "" || !active || submissionBlocked || submitting || queueBusy) return;
    if (/^[/$@]/u.test(nextPrompt) || selectedSkill !== undefined || selectedAgent !== undefined) {
      onNotice(t("queueExtensionBindingUnavailable"), true);
      return;
    }
    setInterruptPrompt(nextPrompt);
  };
  const clearComposer = () => {
      setPrompt("");
      setSelectedSkill(undefined);
      setSelectedAgent(undefined);
      writeDraft(draftKey, "");
  };
  const selectSuggestion = (suggestion: ComposerSuggestion) => {
    if (!suggestion.available) {
      onNotice(suggestion.unavailableReason ?? t("extensionUnavailable"), true);
      return;
    }
    if (suggestion.kind === "skill") {
      const skill = runContext?.extensionCatalog.skills.find((entry) => entry.id === suggestion.id);
      if (skill !== undefined) {
        setSelectedSkill(skill);
        setSelectedAgent(undefined);
        replacePrompt(remainingPromptAfterToken(prompt));
      }
      return;
    }
    if (suggestion.kind === "agent") {
      const agent = runContext?.extensionCatalog.agents.find((entry) => entry.id === suggestion.id);
      if (agent !== undefined) {
        setSelectedAgent(agent);
        setSelectedSkill(undefined);
        replacePrompt(remainingPromptAfterToken(prompt));
      }
      return;
    }
    if (suggestion.completesWithSpace) {
      replacePrompt(`${suggestion.token} `);
      return;
    }
    void executeCommand(suggestion, "").then((completed) => {
      if (completed) clearComposer();
    });
  };
  const executeCommand = async (suggestion: ComposerSuggestion, argument: string) => {
    switch (suggestion.clientAction) {
      case "preview_compaction":
        return onCompact();
      case "open_intent_stack":
        onOpenIntentStack();
        return true;
      case "new_session":
        return onNewSession();
      case "focus_effort": {
        if (argument === "") {
          effortSelectRef.current?.focus();
          return true;
        }
        const selectedOption = runContext?.modelOptions.find(
          (option) => providerModelRefsEqual(
            option.modelRef,
            selectedModelRef ?? runContext.modelRef,
          ),
        );
        if (!selectedOption?.availableReasoningEfforts.includes(argument as ReasoningEffort)) {
          onNotice(t("unsupportedEffort", { value: argument }), true);
          return false;
        }
        onReasoningEffortChange(argument as ReasoningEffort);
        return true;
      }
      case "focus_model": {
        if (argument === "") {
          modelSelectRef.current?.focus();
          return true;
        }
        const exact = runContext?.modelOptions.find(
          (candidate) => providerModelRefKey(candidate.modelRef) === argument,
        );
        const matches = runContext?.modelOptions.filter(
          (candidate) => candidate.modelName === argument || candidate.modelRef.modelId === argument,
        ) ?? [];
        const model = exact ?? (matches.length === 1 ? matches[0] : undefined);
        if (model === undefined || !modelOptionCanBeSelected(model, providerConnections)) {
          onNotice(t("unsupportedModel", { value: argument }), true);
          return false;
        }
        if (!providerModelRefsEqual(model.modelRef, selectedModelRef)) {
          onModelChange(model.modelRef);
        }
        return true;
      }
      case "open_agent_workbench":
        if (suggestion.id === "/plan") {
          const plan = runContext?.extensionCatalog.agents.find((agent) => agent.id === "plan");
          if (plan === undefined || !plan.available || plan.binding === undefined) {
            onNotice(plan?.unavailableReason ?? t("agentExecutionUnavailable"), true);
            return false;
          }
          setSelectedAgent(plan);
          setSelectedSkill(undefined);
          if (argument === "") {
            replacePrompt("");
            return false;
          }
          return onSubmit(argument, undefined, plan.binding);
        }
        onOpenAgentWorkbench(argument);
        return true;
      case "open_session_picker":
        onOpenSessionPicker(argument);
        return true;
      case "open_settings":
        onOpenSettings();
        return true;
      case "open_support":
        onOpenSupport();
        return true;
      default:
        onNotice(suggestion.unavailableReason ?? t("commandUnavailable"), true);
        return false;
    }
  };
  const replacePrompt = (value: string) => {
    setPrompt(value);
    writeDraft(draftKey, value);
    setSuggestionsDismissedFor(undefined);
    setActiveSuggestion(0);
    requestAnimationFrame(() => focusWithoutScroll(composerRef.current));
  };
  const modelOptions = runContext?.modelOptions ?? [];
  const effectiveModelRef = selectedModelRef ?? runContext?.modelRef;
  const modelOption = modelOptions.find((option) =>
    providerModelRefsEqual(option.modelRef, effectiveModelRef));
  const modelName = modelOption?.modelName
    ?? effectiveModelRef?.modelId
    ?? (runContextBusy ? t("loadingModel") : t("modelUnavailable"));
  const connectionFor = (modelRef: ProviderModelRef) =>
    providerConnections?.find((connection) => connection.id === modelRef.connectionId);
  const selectedConnection = effectiveModelRef === undefined
    ? undefined
    : connectionFor(effectiveModelRef);
  const connectionLabel = selectedConnection?.label ?? effectiveModelRef?.connectionId ?? "—";
  const providerLabel = selectedConnection?.providerLabel
    ?? (runContext !== undefined
      && effectiveModelRef?.connectionId === runContext.modelRef.connectionId
      ? runContext.providerName
      : "—");
  const modelOptionValue = (option: ModelOption) => providerModelRefKey(option.modelRef);
  const modelOptionLabel = (option: ModelOption) => {
    const identity = option.displayName === option.modelName
      ? option.modelName
      : `${option.displayName} · ${option.modelName}`;
    const connection = connectionFor(option.modelRef);
    const optionConnection = connection?.label ?? option.modelRef.connectionId;
    const optionProvider = connection?.providerLabel
      ?? (runContext !== undefined
        && option.modelRef.connectionId === runContext.modelRef.connectionId
        ? runContext.providerName
        : undefined);
    const route = optionProvider === undefined
      ? optionConnection
      : `${optionProvider} · ${optionConnection}`;
    const routeIdentity = `${route} · ${identity}`;
    return option.availability === "available"
      ? routeIdentity
      : option.availability === "unverified"
        ? `${routeIdentity} · ${t("modelCatalogUnconfirmed")}`
        : `${routeIdentity} · ${t("unavailable")}`;
  };
  const selectedModelValue = modelOption === undefined ? "" : modelOptionValue(modelOption);
  const modelTooltip = runContext === undefined
    ? modelName
    : modelOption?.availability === "unverified"
      ? t("modelHintUnconfirmed", {
        connection: connectionLabel,
        provider: providerLabel,
        model: modelOption.modelName,
        source: modelProvenanceLabel(modelOption.provenance, t),
      })
      : t("modelHint", {
        connection: connectionLabel,
        provider: providerLabel,
        model: modelOption?.modelName ?? modelName,
      });
  const selectableModelCount = modelOptions.filter(
    (option) => modelOptionCanBeSelected(option, providerConnections),
  ).length;
  const availableReasoningEfforts = modelOption?.availableReasoningEfforts ?? [];
  const permissionModes = runContext?.availablePermissionModes ?? ["read-only", "manual", "auto-edit", "danger-full-access"];
  const activity = activityState === undefined ? undefined : composerActivityCopy(activityState, t);

  return (
    <>
    <form
      className={`composer${activityState === undefined ? "" : ` has-activity activity-${activityState}`}`}
      onSubmit={(event) => { event.preventDefault(); void submit(); }}
    >
      {suggestionsOpen ? (
        <ComposerSuggestions
          id={suggestionListId}
          label={t("composerSuggestions")}
          unavailableLabel={t("unavailable")}
          suggestions={suggestions}
          activeIndex={Math.min(activeSuggestion, suggestions.length - 1)}
          query={suggestionQuery?.query ?? ""}
          onSelect={selectSuggestion}
          onActiveIndexChange={setActiveSuggestion}
        />
      ) : null}
      {activity !== undefined ? (
        <div
          className="composer-activity"
          role={activityState === "connection_error" ? "alert" : "status"}
          aria-live={activityState === "connection_error" ? "assertive" : "polite"}
          aria-atomic="true"
        >
          <span className="composer-activity-indicator" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
          <span className="composer-activity-copy">
            <strong>{activity.label}</strong>
            <small>{activity.detail}</small>
          </span>
        </div>
      ) : null}
      <div className="composer-surface">
        {selectedSkill !== undefined || selectedAgent !== undefined ? (
          <div className="composer-bindings" aria-label={t("activeExtensions")}>
            {selectedSkill !== undefined ? (
              <Button
                className="composer-binding binding-skill"
                variant="quiet"
                type="button"
                onClick={() => setSelectedSkill(undefined)}
                aria-label={t("removeSkill", { name: selectedSkill.name })}
              >
                <span>{selectedSkill.invocationToken}</span>
                <small>{selectedSkill.name}</small>
                <span aria-hidden="true">×</span>
              </Button>
            ) : null}
            {selectedAgent !== undefined ? (
              <Button
                className="composer-binding binding-agent"
                variant="quiet"
                type="button"
                onClick={() => setSelectedAgent(undefined)}
                aria-label={t("removeAgent", { name: selectedAgent.id })}
              >
                <span>{selectedAgent.invocationToken}</span>
                <small>{selectedAgent.description}</small>
                <span aria-hidden="true">×</span>
              </Button>
            ) : null}
          </div>
        ) : null}
        <TextArea
          id="desktop-prompt"
          className="composer-input"
          containerClassName="composer-input-field"
          label={t("messageSigil")}
          labelHidden
          ref={composerRef}
          value={prompt}
          disabled={draftEditingBlocked}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={suggestionsOpen}
          aria-controls={suggestionsOpen ? suggestionListId : undefined}
          aria-activedescendant={suggestionsOpen
            ? `${suggestionListId}-option-${Math.min(activeSuggestion, suggestions.length - 1)}`
            : undefined}
          onPointerDown={(event) => {
            if (document.activeElement !== event.currentTarget) {
              event.preventDefault();
              event.currentTarget.focus({ preventScroll: true });
            }
          }}
          onMouseDown={(event) => {
            if (document.activeElement !== event.currentTarget) {
              event.preventDefault();
              event.currentTarget.focus({ preventScroll: true });
            }
          }}
          onChange={(event) => {
            setPrompt(event.target.value);
            setSuggestionsDismissedFor(undefined);
            setActiveSuggestion(0);
            writeDraft(draftKey, event.target.value);
          }}
          placeholder={
            active
              ? queueSubmissionBlocked ? t("conversationQueueUnavailable") : t("activePrompt")
              : submissionBlocked ? t("readOnlyRecoveryPrompt") : t("prompt")
          }
          rows={1}
          onKeyDown={(event) => {
            if (suggestionsOpen) {
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const delta = event.key === "ArrowDown" ? 1 : -1;
                setActiveSuggestion((current) =>
                  (current + delta + suggestions.length) % suggestions.length,
                );
                return;
              }
              if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) {
                event.preventDefault();
                selectSuggestion(suggestions[Math.min(activeSuggestion, suggestions.length - 1)]);
                return;
              }
              if (event.key === "Escape") {
                event.preventDefault();
                setSuggestionsDismissedFor(suggestionQuery?.token);
                return;
              }
            }
            if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
            event.preventDefault();
            void submit();
          }}
        />
        <div className="composer-toolbar">
          <div className="composer-options">
            <Tooltip label={modelTooltip}>
              <div className="composer-model">
                <Icon name="model" />
                <Select
                  label={t("model")}
                  labelHidden
                  containerClassName="composer-model-field"
                  className="composer-model-select"
                  ref={modelSelectRef}
                  value={selectedModelValue}
                  disabled={active || submissionBlocked || runContextBusy || selectableModelCount === 0}
                  onChange={(event) => {
                    const selected = modelOptions.find(
                      (option) => modelOptionValue(option) === event.target.value,
                    );
                    if (selected !== undefined && modelOptionCanBeSelected(selected, providerConnections)) {
                      onModelChange(selected.modelRef);
                    }
                  }}
                >
                  {modelOptions.length === 0 ? <option value="">{modelName}</option> : modelOptions.map((option) => (
                    <option
                      key={modelOptionValue(option)}
                      value={modelOptionValue(option)}
                      disabled={!modelOptionCanBeSelected(option, providerConnections)}
                    >
                      {modelOptionLabel(option)}
                    </option>
                  ))}
                </Select>
              </div>
            </Tooltip>
            <div className="composer-mode">
              <Icon name="shield" />
              <Select
                label={t("permissionMode")}
                labelHidden
                containerClassName="composer-mode-field"
                className="composer-mode-select"
                value={permissionMode}
                disabled={active || submissionBlocked || runContextBusy}
                onChange={(event) => onPermissionModeChange(event.target.value as PermissionMode)}
              >
                {permissionModes.map((mode) => (
                  <option key={mode} value={mode}>{permissionModeLabel(mode, t)}</option>
                ))}
              </Select>
            </div>
            {availableReasoningEfforts.length > 0 ? (
              <div className="composer-effort">
                <Select
                  label={t("reasoningEffort")}
                  labelHidden
                  containerClassName="composer-effort-field"
                  className="composer-effort-select"
                  ref={effortSelectRef}
                  value={reasoningEffort ?? ""}
                  disabled={active || submissionBlocked || runContextBusy}
                  onChange={(event) => onReasoningEffortChange(event.target.value as ReasoningEffort)}
                >
                  {reasoningEffort === undefined ? <option value="">{t("effortUnavailable")}</option> : null}
                  {availableReasoningEfforts.map((effort) => (
                    <option key={effort} value={effort}>{reasoningEffortLabel(effort, t)}</option>
                  ))}
                </Select>
              </div>
            ) : null}
            <ContextUsage context={runContext} loading={runContextBusy} />
          </div>
          <div className="composer-actions">
            <Popover
              className="composer-queue-popover"
              accessibleLabel={t("openConversationQueueCount", { count: queueCount })}
              align="end"
              label={(
                <span className={`composer-queue-trigger${queueCount > 0 ? " has-items" : ""}`}>
                  <Icon name="queue" />
                  {queueCount > 0 ? <span className="composer-queue-count" aria-hidden="true">{queueCount}</span> : null}
                </span>
              )}
              onOpenChange={(open) => {
                if (open) onOpenQueue();
              }}
            >
              {queuePanel}
            </Popover>
          {active ? (
            <>
              <Tooltip label={t("interruptAndRunNextHint")}>
                <IconButton
                  className="composer-submit composer-interrupt-next"
                  type="button"
                  aria-label={t("interruptAndRunNext")}
                  icon={<Icon name="interrupt-next" />}
                  disabled={prompt.trim() === "" || submissionBlocked || submitting || queueBusy}
                  onClick={requestInterruptAndRunNext}
                />
              </Tooltip>
              <Tooltip label={submissionBlocked ? t("liveControlsUnavailable") : t("stopRunHint")}>
                <IconButton
                  className="composer-submit composer-stop"
                  type="button"
                  aria-label={t("stopRun")}
                  icon={<Icon name="stop" />}
                  disabled={controlBusy || submissionBlocked}
                  onClick={onCancel}
                />
              </Tooltip>
              <Tooltip label={queueBusy ? t("queueingMessage") : t("queueMessageHint")}>
                <IconButton
                  className="composer-submit sg-icon-button-primary"
                  type="submit"
                  aria-label={t("queueMessage")}
                  icon={<Icon name="queue" />}
                  disabled={prompt.trim() === "" || queueSubmissionBlocked || submitting || queueBusy}
                  aria-busy={queueBusy || undefined}
                />
              </Tooltip>
            </>
          ) : (
            <Tooltip label={submissionBlocked ? t("sendBlockedUntilContinuityChecked") : submitting ? t("startingRun") : t("sendMessageHint")}>
              <IconButton
                className="composer-submit sg-icon-button-primary"
                type="submit"
                aria-label={t("sendMessage")}
                icon={<Icon name="send" />}
                disabled={prompt.trim() === "" || submissionBlocked || submitting}
                aria-busy={submitting || undefined}
              />
            </Tooltip>
          )}
          </div>
        </div>
      </div>
    </form>
    <Dialog
      open={interruptPrompt !== undefined}
      title={t("interruptAndRunNextQuestion")}
      description={t("interruptAndRunNextDetail")}
      returnFocusRef={composerRef}
      onOpenChange={(open) => {
        if (!open && !queueBusy) setInterruptPrompt(undefined);
      }}
    >
      <p className="interrupt-next-preview">{interruptPrompt}</p>
      <div className="confirmation-actions">
        <Button type="button" data-initial-focus disabled={queueBusy} onClick={() => setInterruptPrompt(undefined)}>
          {t("keepCurrentRun")}
        </Button>
        <Button
          type="button"
          variant="danger"
          busy={queueBusy}
          onClick={() => {
            if (interruptPrompt === undefined) return;
            void onInterruptAndRunNext(interruptPrompt).then((completed) => {
              if (!completed) return;
              setInterruptPrompt(undefined);
              clearComposer();
            });
          }}
        >
          {t("interruptAndRunNext")}
        </Button>
      </div>
    </Dialog>
    </>
  );
}

function focusWithoutScroll(element: HTMLElement | null): void {
  element?.focus({ preventScroll: true });
}

function composerActivityCopy(state: ComposerActivityState, t: ReturnType<typeof useLocale>["t"]) {
  switch (state) {
    case "starting":
      return { label: t("composerActivityStarting"), detail: t("composerActivityStartingDetail") };
    case "connecting":
      return { label: t("composerActivityConnecting"), detail: t("composerActivityConnectingDetail") };
    case "running":
      return { label: t("composerActivityRunning"), detail: t("composerActivityRunningDetail") };
    case "waiting_for_approval":
      return { label: t("composerActivityApproval"), detail: t("composerActivityApprovalDetail") };
    case "stopping":
      return { label: t("composerActivityStopping"), detail: t("composerActivityStoppingDetail") };
    case "recovering":
      return { label: t("composerActivityRecovering"), detail: t("composerActivityRecoveringDetail") };
    case "reconnecting":
      return { label: t("composerActivityReconnecting"), detail: t("composerActivityReconnectingDetail") };
    case "connection_error":
      return { label: t("composerActivityConnectionError"), detail: t("composerActivityConnectionErrorDetail") };
    case "finalizing":
      return { label: t("composerActivityFinalizing"), detail: t("composerActivityFinalizingDetail") };
  }
}

function modelProvenanceLabel(
  provenance: ModelOption["provenance"],
  t: Translate,
): string {
  switch (provenance) {
    case "remote": return t("modelProvenance_remote");
    case "cache": return t("modelProvenance_cache");
    case "bundled": return t("modelProvenance_bundled");
    case "configured": return t("modelProvenance_configured");
    case "manual": return t("modelProvenance_manual");
  }
}

function modelOptionCanBeSelected(
  option: ModelOption,
  connections: ProviderConnection[] | undefined,
): boolean {
  if (!modelOptionIsSelectable(option)) return false;
  if (connections === undefined) return true;
  const connection = connections.find(
    (candidate) => candidate.id === option.modelRef.connectionId,
  );
  return connection !== undefined && ["ready", "unverified"].includes(connection.readiness);
}

interface LeadingInvocationToken {
  token: string;
  query: string;
  kind: "command" | "skill" | "agent";
}

function leadingInvocationToken(prompt: string): LeadingInvocationToken | undefined {
  const match = prompt.match(/^\s*([/$@][^\s]*)$/u);
  const token = match?.[1];
  if (token === undefined) return undefined;
  const kind = token.startsWith("/") ? "command" : token.startsWith("$") ? "skill" : "agent";
  return { token, query: token, kind };
}

function buildSuggestions(
  context: RunContext | undefined,
  leading: LeadingInvocationToken | undefined,
  selectedSkill: SkillCatalogEntry | undefined,
): ComposerSuggestion[] {
  if (context === undefined || leading === undefined) return [];
  const query = leading.query.toLocaleLowerCase();
  if (leading.kind === "command") {
    const commands = context.extensionCatalog.commands
      .filter((entry) => entry.available)
      .filter((entry) =>
        entry.canonical.toLocaleLowerCase().includes(query) ||
        entry.aliases.some((alias) => alias.toLocaleLowerCase().includes(query)),
      )
      .map(commandSuggestion);
    const skills = context.extensionCatalog.skills
      .filter((entry) => entry.invocationToken.startsWith("/"))
      .filter((entry) =>
        entry.invocationToken.toLocaleLowerCase().includes(query) ||
        entry.name.toLocaleLowerCase().includes(query.slice(1)),
      )
      .map(skillSuggestion);
    return [...commands, ...skills];
  }
  if (leading.kind === "skill" && selectedSkill === undefined) {
    return context.extensionCatalog.skills
      .filter((entry) => entry.invocationToken.startsWith("$"))
      .filter((entry) =>
        entry.invocationToken.toLocaleLowerCase().includes(query) ||
        entry.name.toLocaleLowerCase().includes(query.slice(1)),
      )
      .map(skillSuggestion);
  }
  if (leading.kind === "agent") {
    return context.extensionCatalog.agents
      .filter((entry) =>
        entry.invocationToken.toLocaleLowerCase().includes(query) ||
        entry.id.toLocaleLowerCase().includes(query.slice(1)) ||
        entry.name.toLocaleLowerCase().includes(query.slice(1)),
      )
      .map((entry) => ({
        id: entry.id,
        token: entry.invocationToken,
        label: entry.name,
        description: entry.description,
        kind: "agent" as const,
        available: entry.available && entry.binding !== undefined,
        unavailableReason: entry.unavailableReason,
      }));
  }
  return [];
}

function skillSuggestion(entry: SkillCatalogEntry): ComposerSuggestion {
  return {
    id: entry.id,
    token: entry.invocationToken,
    label: entry.name,
    description: entry.description,
    kind: "skill",
    available: entry.available && entry.binding !== undefined,
    unavailableReason: entry.unavailableReason,
  };
}

function commandSuggestion(entry: RunContext["extensionCatalog"]["commands"][number]): ComposerSuggestion {
  return {
    id: entry.canonical,
    token: entry.canonical,
    label: entry.label,
    description: entry.description,
    kind: "command",
    available: entry.available,
    unavailableReason: entry.unavailableReason,
    completesWithSpace: entry.completesWithSpace,
    clientAction: entry.clientAction,
  };
}

function resolveCommand(context: RunContext | undefined, prompt: string) {
  if (context === undefined || !prompt.startsWith("/")) return undefined;
  const [token, ...argumentParts] = prompt.split(/\s+/u);
  const entry = context.extensionCatalog.commands.find(
    (candidate) => candidate.canonical === token || candidate.aliases.includes(token),
  );
  if (entry === undefined) return undefined;
  return { suggestion: commandSuggestion(entry), argument: argumentParts.join(" ").trim() };
}

function resolveSkill(context: RunContext | undefined, prompt: string) {
  if (context === undefined || !/^[/$]/u.test(prompt)) return undefined;
  const match = prompt.match(/^([/$][^\s]+)\s+([\s\S]+)$/u);
  if (match === null) return undefined;
  const skill = context.extensionCatalog.skills.find(
    (entry) => entry.invocationToken === match[1],
  );
  return skill === undefined ? undefined : { skill, prompt: match[2].trim() };
}

function resolveAgent(context: RunContext | undefined, prompt: string) {
  if (context === undefined || !prompt.startsWith("@")) return undefined;
  const match = prompt.match(/^@([^\s]+)\s+([\s\S]+)$/u);
  if (match === null) return undefined;
  const token = `@${match[1]}`;
  const agent = context.extensionCatalog.agents.find(
    (entry) => entry.invocationToken === token,
  );
  return agent === undefined ? undefined : { agent, prompt: match[2].trim() };
}

function remainingPromptAfterToken(prompt: string): string {
  return prompt.replace(/^\s*[/$@][^\s]*\s*/u, "");
}

function ContextUsage({ context, loading }: { context?: RunContext; loading: boolean }) {
  const { t } = useLocale();
  const used = context?.lastPromptTokens;
  const limit = context?.contextWindowTokens;
  if (loading) {
    return <span className="context-usage context-unavailable" aria-label={t("contextLoading")}>{t("contextLoading")}</span>;
  }
  if (used === undefined || limit === undefined || limit === 0) {
    return <span className="context-usage context-unavailable" aria-label={t("contextUnavailable")}>{t("contextUnavailable")}</span>;
  }
  const boundedUsed = Math.min(used, limit);
  const ratio = boundedUsed / limit;
  const percent = ratio * 100;
  const percentLabel = percent < 1 ? percent.toFixed(1) : Math.round(percent).toString();
  const style = { "--context-used": `${ratio * 100}%` } as CSSProperties;
  return (
    <Tooltip label={t("contextTokens", { used: formatTokens(used), limit: formatTokens(limit) })}>
      <span
        className="context-usage"
        role="meter"
        aria-label={t("contextUsage", { percent: percentLabel })}
        aria-valuemin={0}
        aria-valuemax={limit}
        aria-valuenow={boundedUsed}
        style={style}
      >
        <span className="context-track" aria-hidden="true"><span /></span>
        <span>{percentLabel}%</span>
      </span>
    </Tooltip>
  );
}

function permissionModeLabel(mode: PermissionMode, t: ReturnType<typeof useLocale>["t"]): string {
  switch (mode) {
    case "read-only": return t("readOnly");
    case "manual": return t("manual");
    case "auto-edit": return t("autoEdit");
    case "danger-full-access": return t("fullAccess");
  }
}

function reasoningEffortLabel(
  effort: ReasoningEffort,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (effort) {
    case "low": return t("effortLow");
    case "medium": return t("effortMedium");
    case "high": return t("effortHigh");
    case "max": return t("effortMax");
  }
}

function formatTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 === 0 ? 0 : 1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 === 0 ? 0 : 1)}K`;
  return value.toString();
}

export function draftStorageKey(workspaceId: string, sessionId: string): string {
  return `sigil:conversation-draft:v1:${workspaceId}:${sessionId}`;
}

function readDraft(key: string): string {
  try {
    return window.localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function writeDraft(key: string, value: string): void {
  try {
    if (new TextEncoder().encode(value).byteLength > MAX_DRAFT_BYTES) {
      window.localStorage.removeItem(key);
      return;
    }
    if (value === "") window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
  } catch {
    // Draft persistence is best-effort; the controlled input remains usable.
  }
}
