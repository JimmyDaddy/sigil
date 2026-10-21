import {
  useDeferredValue,
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

import { SafeMarkdown } from "./SafeMarkdown";
import { MarkdownEditor, type MarkdownEditorHandle } from "./ui/editor/MarkdownEditor";
import type { MarkdownFormat } from "./ui/editor/markdownEditing";
import { ComposerSuggestions, type ComposerSuggestion } from "./ComposerSuggestions";
import type { ComposerActivityState } from "./features/conversation/composerActivity";
import { presentComposerActivity, type ProductStatusPresentation } from "./statusPresentation";
import { modelOptionIsSelectable, providerModelRefKey, providerModelRefsEqual } from "./types";
import type {
  DraftImage,
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
import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { Button, Dialog, IconButton, Popover, Select, Tooltip } from "./ui/primitives";

const MAX_DRAFT_BYTES = 256 * 1024;

export function Composer({
  draftKey,
  persistentDraftKey,
  active,
  submissionBlocked,
  stopControlBlocked = false,
  reviewActive = false,
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
  statusPresentation,
  showActivity = true,
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
  onCancel,
  onPickImage,
  onIngestImage,
  onReleaseImages,
}: {
  draftKey: string;
  persistentDraftKey?: string;
  active: boolean;
  submissionBlocked: boolean;
  stopControlBlocked?: boolean;
  reviewActive?: boolean;
  submitting: boolean;
  controlBusy: boolean;
  composerRef: RefObject<HTMLElement | null>;
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
  statusPresentation?: ProductStatusPresentation;
  showActivity?: boolean;
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
  onSubmit: (prompt: string, skillBinding?: SkillBinding, agentBinding?: AgentBinding, images?: DraftImage[]) => Promise<boolean>;
  onPickImage?: () => Promise<DraftImage | null>;
  onIngestImage?: (bytes: number[]) => Promise<DraftImage>;
  onReleaseImages?: (handles: string[]) => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useLocale();
  const storageKey = persistentDraftKey ?? draftKey;
  const storageKeyRef = useRef(storageKey);
  storageKeyRef.current = storageKey;
  const [prompt, setPrompt] = useState(() => readDraft(storageKey));
  const [draftUnsaved, setDraftUnsaved] = useState(false);
  const editorRef = useRef<MarkdownEditorHandle>(null);
  const [previewVisible, setPreviewVisible] = useState(false);
  const [editorExpanded, setEditorExpanded] = useState(false);
  const previewRef = useRef<HTMLElement>(null);
  const previewPosition = useRef({ draftKey, top: 0 });
  const draftEmpty = prompt === "";
  useLayoutEffect(() => {
    // A late persistence key still describes this draft; only its identity or clear resets reading.
    if (previewPosition.current.draftKey !== draftKey || draftEmpty) {
      previewPosition.current = { draftKey, top: 0 };
    }
    if (previewRef.current !== null) previewRef.current.scrollTop = previewPosition.current.top;
  }, [draftKey, draftEmpty, previewVisible, editorExpanded, reviewActive]);
  const collapseEditor = () => {
    setEditorExpanded(false);
    editorRef.current?.focus();
  };
  const [formatOpen, setFormatOpen] = useState(false);
  const appleShortcuts = typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");
  const previewText = useDeferredValue(prompt);
  const previousStorageKey = useRef(storageKey);
  useLayoutEffect(() => {
    if (previousStorageKey.current === storageKey) return;
    // Catalog identity can arrive after a new conversation's input is already mounted.
    // Move only its local text; changing persistence must not remount the editor or images.
    const saved = writeDraft(storageKey, prompt);
    setDraftUnsaved(!saved);
    if (saved) {
      writeDraft(previousStorageKey.current, "");
      previousStorageKey.current = storageKey;
    }
  }, [prompt, storageKey]);
  const draftRevision = useRef(0);
  const [images, setImages] = useState<DraftImage[]>([]);
  const imagesRef = useRef<DraftImage[]>([]);
  const [imageBusy, setImageBusy] = useState(false);
  const imagePending = useRef(false);
  const imageScope = useRef(0);
  const releaseImagesRef = useRef(onReleaseImages);
  releaseImagesRef.current = onReleaseImages;
  useEffect(() => () => {
    imageScope.current += 1;
    const handles = imagesRef.current.map((image) => image.attachmentId);
    if (handles.length > 0) void releaseImagesRef.current?.(handles).catch(() => {});
    imagesRef.current = [];
  }, [draftKey]);
  const removeImage = (id: string) => {
    draftRevision.current += 1;
    imagesRef.current = imagesRef.current.filter((image) => image.attachmentId !== id);
    setImages(imagesRef.current);
    void onReleaseImages?.([id]).catch(() => {});
  };
  const addImage = async (load: () => Promise<DraftImage | null>) => {
    if (imagePending.current) return;
    if (active || imagesRef.current.length >= 4) { onNotice(t(active ? "imageForegroundOnly" : "imageCountLimit"), true); return; }
    const scope = imageScope.current;
    imagePending.current = true; setImageBusy(true);
    try {
      const image = await load();
      if (image === null) return;
      if (scope !== imageScope.current) {
        void onReleaseImages?.([image.attachmentId]).catch(() => {}); return;
      }
      if (imagesRef.current.reduce((sum, entry) => sum + entry.byteLen, 0) + image.byteLen > 24 * 1024 * 1024) {
        void onReleaseImages?.([image.attachmentId]).catch(() => {});
        onNotice(t("imageSizeLimit"), true); return;
      }
      draftRevision.current += 1;
      imagesRef.current = [...imagesRef.current, image]; setImages(imagesRef.current);
    } catch { if (scope === imageScope.current) onNotice(t("imageAttachFailed"), true); }
    finally { imagePending.current = false; if (scope === imageScope.current) setImageBusy(false); }
  };

  const [selectedSkill, setSelectedSkill] = useState<SkillCatalogEntry>();
  const [selectedAgent, setSelectedAgent] = useState<AgentCatalogEntry>();
  const [activeSuggestion, setActiveSuggestion] = useState(0);
  const [suggestionsDismissedFor, setSuggestionsDismissedFor] = useState<string>();
  const suggestionListId = `${useId()}-suggestions`;
  const previewId = `${useId()}-preview`;
  const modelSelectRef = useRef<HTMLSelectElement>(null);
  const effortSelectRef = useRef<HTMLSelectElement>(null);
  useEffect(() => {
    if (requestedSkill === undefined) return;
    draftRevision.current += 1;
    setSelectedSkill(requestedSkill);
    setSelectedAgent(undefined);
    requestAnimationFrame(() => focusWithoutScroll(composerRef.current));
  }, [composerRef, requestedSkill]);
  useEffect(() => {
    if (requestedAgent === undefined) return;
    draftRevision.current += 1;
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


  const submit = async () => {
    const submittedRevision = draftRevision.current;
    let nextPrompt = prompt;
    if (
      (nextPrompt.trim() === "" && images.length === 0)
      || (!active && submissionBlocked)
      || submitting
      || imagePending.current
      || (active && queueBusy)
    ) return;
    const command = resolveCommand(runContext, nextPrompt.trim());
    if (images.length > 0 && (active || selectedAgent !== undefined)) {
      onNotice(t("imageForegroundOnly"), true); return;
    }
    if (command !== undefined) {
      if (await executeCommand(command.suggestion, command.argument)) clearComposer(submittedRevision);
      return;
    }
    const directSkill = selectedSkill === undefined && selectedAgent === undefined
      ? resolveSkill(runContext, nextPrompt.trim())
      : undefined;
    const skill = selectedSkill ?? directSkill?.skill;
    if (directSkill !== undefined) nextPrompt = directSkill.prompt;
    const directAgent = selectedAgent === undefined && skill === undefined
      ? resolveAgent(runContext, nextPrompt.trim())
      : undefined;
    const agent = selectedAgent ?? directAgent?.agent;
    if (directAgent !== undefined) nextPrompt = directAgent.prompt;
    if (nextPrompt === "" && images.length === 0) {
      onNotice(t(agent === undefined ? "skillNeedsPrompt" : "agentNeedsPrompt"), true);
      return;
    }
    if (skill !== undefined && (!skill.available || skill.binding === undefined)) {
      onNotice(skill.unavailableReason ?? t("extensionUnavailable"), true);
      return;
    }
    if (nextPrompt.trimStart().startsWith("@") && agent === undefined) {
      onNotice(t("agentExecutionUnavailable"), true);
      return;
    }
    if (agent !== undefined && (!agent.available || agent.binding === undefined)) {
      onNotice(agent.unavailableReason ?? t("agentExecutionUnavailable"), true);
      return;
    }
    if (images.length > 0 && agent !== undefined) { onNotice(t("imageForegroundOnly"), true); return; }
    if (active && (skill !== undefined || agent !== undefined)) {
      onNotice(t("queueExtensionBindingUnavailable"), true);
      return;
    }
    const accepted = images.length === 0
      ? await onSubmit(nextPrompt, skill?.binding, agent?.binding)
      : await onSubmit(nextPrompt, skill?.binding, agent?.binding, images);
    if (accepted) {
      clearComposer(submittedRevision);
    }
  };
  const clearComposer = (expectedRevision = draftRevision.current) => {
    // An accepted request owns only the draft that was submitted, not edits made while waiting.
    if (draftRevision.current !== expectedRevision) return;
    draftRevision.current += 1;
    setPrompt("");
    const handles = imagesRef.current.map((image) => image.attachmentId);
    imagesRef.current = []; setImages([]);
    if (handles.length > 0) void onReleaseImages?.(handles).catch(() => {});
    setSelectedSkill(undefined);
    setSelectedAgent(undefined);
    setDraftUnsaved(!writeDraft(storageKeyRef.current, ""));
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
    const submittedRevision = draftRevision.current;
    void executeCommand(suggestion, "").then((completed) => {
      if (completed) clearComposer(submittedRevision);
    });
  };
  const executeCommand = async (suggestion: ComposerSuggestion, argument: string) => {
    if (imagesRef.current.length > 0) {
      onNotice(t("imageForegroundOnly"), true);
      return false;
    }
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
        if (model === undefined || !modelOptionIsSelectable(model)) {
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
    draftRevision.current += 1;
    setPrompt(value);
    setDraftUnsaved(!writeDraft(storageKeyRef.current, value));
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
    const identity = option.modelName;
    const connection = connectionFor(option.modelRef);
    const optionConnection = connection?.label ?? option.modelRef.connectionId;
    const multipleConnections = new Set(modelOptions.map((candidate) => candidate.modelRef.connectionId)).size > 1;
    const routeIdentity = multipleConnections ? `${optionConnection} · ${identity}` : identity;
    return option.availability === "configured_unavailable"
      ? `${routeIdentity} · ${t("modelNotListed")}`
      : routeIdentity;
  };
  const selectedModelValue = modelOption === undefined ? "" : modelOptionValue(modelOption);
  const modelTooltip = runContext === undefined
    ? modelName
    : t("modelHint", {
      connection: connectionLabel,
      provider: providerLabel,
      model: modelOption?.modelName ?? modelName,
    });
  const selectableModelCount = modelOptions.filter(
    modelOptionIsSelectable,
  ).length;
  const availableReasoningEfforts = modelOption?.availableReasoningEfforts ?? [];
  const permissionModes = runContext?.availablePermissionModes ?? ["read-only", "manual", "auto-edit", "danger-full-access"];
  const activity = statusPresentation
    ?? (activityState === undefined ? undefined : presentComposerActivity(activityState, t));
  const expandedWriting = editorExpanded && !reviewActive;
  const suggestionList = suggestionsOpen ? (
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
  ) : null;

  return (
    <>
    <form
      className={`composer${activity === undefined || !showActivity
        ? ""
        : ` has-activity${activityState === undefined ? "" : ` activity-${activityState}`}`}`}
      data-composer-product-status={activity?.status}
      data-expanded={editorExpanded}
      data-review-active={reviewActive}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || event.defaultPrevented || event.nativeEvent.defaultPrevented
          || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
        if (suggestionsOpen) {
          event.preventDefault();
          setSuggestionsDismissedFor(suggestionQuery?.token);
        } else if (editorExpanded) {
          event.preventDefault();
          collapseEditor();
        }
      }}
      onSubmit={(event) => {
        if (event.target !== event.currentTarget) return;
        event.preventDefault();
        void submit();
      }}
    >
      {!expandedWriting ? suggestionList : null}
      {activity !== undefined && showActivity ? (
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
            <small>{activity.message}</small>
          </span>
        </div>
      ) : null}
      <div className="composer-surface">
        {images.length > 0 ? <div className="composer-images" aria-label={t("attachedImages")}>
          {images.map((image, index) => <figure key={image.attachmentId}>
            <img src={image.previewDataUrl} alt={t("imageNumber", { count: index + 1 })} />
            <figcaption>{image.width} × {image.height}</figcaption>
            <Button type="button" variant="quiet" onClick={() => removeImage(image.attachmentId)} aria-label={t("removeImage", { count: index + 1 })}>×</Button>
          </figure>)}
        </div> : null}
        {selectedSkill !== undefined || selectedAgent !== undefined ? (
          <div className="composer-bindings" aria-label={t("activeExtensions")}>
            {selectedSkill !== undefined ? (
              <Button
                className="composer-binding binding-skill"
                variant="quiet"
                type="button"
                onClick={() => { draftRevision.current += 1; setSelectedSkill(undefined); }}
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
                onClick={() => { draftRevision.current += 1; setSelectedAgent(undefined); }}
                aria-label={t("removeAgent", { name: selectedAgent.id })}
              >
                <span>{selectedAgent.invocationToken}</span>
                <small>{selectedAgent.description}</small>
                <span aria-hidden="true">×</span>
              </Button>
            ) : null}
          </div>
        ) : null}
        {draftUnsaved ? <small role="status" className="composer-draft-warning">{t("draftNotSaved")}</small> : null}
        <div className="composer-editing-tools">
          {onPickImage !== undefined ? <IconButton
            type="button"
            aria-label={imageBusy ? t("attachingImage") : t("attachImage")}
            title={imageBusy ? t("attachingImage") : t("attachImage")}
            aria-busy={imageBusy || undefined}
            disabled={active || submitting || imageBusy}
            onClick={() => void addImage(onPickImage)}
            icon={<Icon name="image" />}
          /> : null}
          <Popover label={t("markdownFormat")} accessibleLabel={t("markdownFormat")} className="composer-format" open={formatOpen} onOpenChange={setFormatOpen} align="start">
            <div className="composer-format-actions">
              {(["bold", "italic", "code", "code_block", "list", "link"] as const).map((format) => {
                const shortcut = formatShortcuts[format];
                return <Button type="button" variant="quiet" key={format}
                  aria-keyshortcuts={shortcut === undefined ? undefined : `${appleShortcuts ? "Meta" : "Control"}+${shortcut}`}
                  onClick={() => {
                    setFormatOpen(false);
                    editorRef.current?.format(format);
                    setPreviewVisible(true);
                  }}>
                  <span>{t(formatLabels[format])}</span>
                  {shortcut === undefined ? null : <kbd aria-hidden="true">{appleShortcuts ? `⌘${shortcut}` : `Ctrl+${shortcut}`}</kbd>}
                </Button>;
              })}
            </div>
          </Popover>
          <Button type="button" variant="quiet" aria-pressed={previewVisible} aria-controls={previewVisible ? previewId : undefined} onClick={() => setPreviewVisible((visible) => !visible)}>{t("markdownPreview")}</Button>
          <Button type="button" variant="quiet" aria-expanded={editorExpanded} onClick={() => {
            setEditorExpanded((expanded) => !expanded);
            editorRef.current?.focus();
          }}>{t(editorExpanded ? "collapseEditor" : "expandEditor")}</Button>
          <small className="composer-editing-hint">{t("markdownSendHint")}</small>
        </div>
        <div className="composer-document" data-preview={previewVisible} data-expanded={editorExpanded} data-review-active={reviewActive}>
          {expandedWriting ? suggestionList : null}
          <MarkdownEditor
            ref={editorRef}
            value={prompt}
            inputRef={composerRef}
            label={t("messageSigil")}
            expanded={editorExpanded}
            reviewActive={reviewActive}
            suggestionsOpen={suggestionsOpen}
            suggestionListId={suggestionsOpen ? suggestionListId : undefined}
            activeSuggestionId={suggestionsOpen ? `${suggestionListId}-option-${Math.min(activeSuggestion, suggestions.length - 1)}` : undefined}
            onCompositionStart={() => { draftRevision.current += 1; }}
            onPaste={(event) => {
              const file = Array.from(event.clipboardData?.files ?? []).find((candidate) => candidate.type.startsWith("image/"));
              if (file === undefined || onIngestImage === undefined) return;
              event.preventDefault();
              if (file.size > 8 * 1024 * 1024) { onNotice(t("imageSizeLimit"), true); return; }
              void addImage(async () => onIngestImage(Array.from(new Uint8Array(await file.arrayBuffer()))));
            }}
            onChange={(value) => {
              draftRevision.current += 1;
              setPrompt(value);
              setSuggestionsDismissedFor(undefined);
              setActiveSuggestion(0);
              setDraftUnsaved(!writeDraft(storageKeyRef.current, value));
            }}
            placeholder={active ? t("activePrompt") : submissionBlocked ? t("readOnlyRecoveryPrompt") : t("prompt")}
            onKeyDown={(event, composing) => {
              if (composing) return;
              if (suggestionsOpen) {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  const delta = event.key === "ArrowDown" ? 1 : -1;
                  setActiveSuggestion((current) => (current + delta + suggestions.length) % suggestions.length);
                  return;
                }
                if ((event.key === "Tab" || event.key === "Enter") && !event.shiftKey) {
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
              if (event.key === "Escape" && editorExpanded) {
                event.preventDefault();
                collapseEditor();
                return;
              }
              if (event.key !== "Enter" || event.shiftKey) return;
              event.preventDefault();
              void submit();
            }}
          />
          {previewVisible ? <section ref={previewRef} id={previewId} className="composer-preview" aria-label={t("markdownPreview")} tabIndex={0}
            onScroll={(event) => {
              previewPosition.current = { draftKey, top: draftEmpty ? 0 : event.currentTarget.scrollTop };
            }}>
            {previewText === "" ? <p className="composer-preview-empty">{t("markdownPreviewEmpty")}</p>
              : <SafeMarkdown text={previewText} contentId={`composer-preview:${draftKey}`} codeBlockVariant="embedded" />}
          </section> : null}
        </div>
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
                    if (selected !== undefined && modelOptionIsSelectable(selected)) {
                      onModelChange(selected.modelRef);
                    }
                  }}
                >
                  {modelOptions.length === 0 ? <option value="">{modelName}</option> : modelOptions.map((option) => (
                    <option
                      key={modelOptionValue(option)}
                      value={modelOptionValue(option)}
                      disabled={!modelOptionIsSelectable(option)}
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
              <Tooltip label={stopControlBlocked ? t("liveControlsUnavailable") : t("stopRunHint")}>
                <IconButton
                  className="composer-submit composer-stop"
                  type="button"
                  aria-label={t("stopRun")}
                  icon={<Icon name="stop" />}
                  disabled={controlBusy || stopControlBlocked}
                  onClick={onCancel}
                />
              </Tooltip>
              <Tooltip label={queueBusy ? t("queueingMessage") : t("queueMessageHint")}>
                <IconButton
                  className="composer-submit sg-icon-button-primary"
                  type="submit"
                  aria-label={t("queueMessage")}
                  icon={<Icon name="queue" />}
                  disabled={prompt.trim() === "" || submitting || queueBusy}
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
                disabled={(prompt.trim() === "" && images.length === 0) || imageBusy || submissionBlocked || submitting}
                aria-busy={submitting || undefined}
              />
            </Tooltip>
          )}
          </div>
        </div>
      </div>
    </form>
    </>
  );
}

function focusWithoutScroll(element: HTMLElement | null): void {
  element?.focus({ preventScroll: true });
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
  if (loading) return null;
  if (used === undefined || limit === undefined || limit === 0) {
    const cache = context?.cacheUsage;
    if (cache === undefined || cache.cacheReadTokens + cache.cacheMissTokens === 0) return null;
    return <Popover className="composer-context" accessibleLabel={t("contextDetails")}
      label={<Icon name="stack" />}><CacheUsage context={context} loading={false} /></Popover>;
  }
  const boundedUsed = Math.min(used, limit);
  const ratio = boundedUsed / limit;
  const percent = ratio * 100;
  const percentLabel = percent < 1 ? percent.toFixed(1) : Math.round(percent).toString();
  const style = { "--context-used": `${ratio * 100}%` } as CSSProperties;
  return (
    <Popover className="composer-context" accessibleLabel={t("contextDetails")}
      label={<span><Icon name="stack" />{percentLabel}%</span>}>
      <strong>{t("contextTokens", { used: formatTokens(used), limit: formatTokens(limit) })}</strong>
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
      <CacheUsage context={context} loading={loading} />
    </Popover>
  );
}

function CacheUsage({ context, loading }: { context?: RunContext; loading: boolean }) {
  const { t } = useLocale();
  const usage = context?.cacheUsage;
  if (loading || usage === undefined) return null;
  const observed = usage.cacheReadTokens + usage.cacheMissTokens;
  if (observed === 0) return null;
  const ratio = usage.cacheReadTokens / observed;
  const percent = ratio * 100;
  const percentLabel = percent < 1 ? percent.toFixed(1) : Math.round(percent).toString();
  const write = usage.cacheWriteTokens === undefined ? "—" : formatTokens(usage.cacheWriteTokens);
  const mutation = usage.lastLayoutMutation ?? "—";
  const tokenDetails = t("cacheTokens", {
    read: formatTokens(usage.cacheReadTokens),
    miss: formatTokens(usage.cacheMissTokens),
    write,
    mutation,
  });
  const diagnostic = usage.providerMissWithoutLocalMutation
    ? `${tokenDetails} · ${t("cacheProviderMissDiagnostic")}`
    : tokenDetails;
  return (
    <Tooltip label={diagnostic}>
      <span
        className={`context-usage cache-usage${usage.providerMissWithoutLocalMutation ? " cache-warning" : ""}`}
        aria-label={t("cacheUsage", { percent: percentLabel })}
      >
        {t("cacheBadge", { percent: percentLabel })}
      </span>
    </Tooltip>
  );
}

const formatLabels = {
  bold: "markdownBold", italic: "markdownItalic", code: "markdownCode",
  code_block: "markdownCodeBlock", list: "markdownList", link: "markdownLink",
} as const satisfies Record<MarkdownFormat, Parameters<ReturnType<typeof useLocale>["t"]>[0]>;

const formatShortcuts: Partial<Record<keyof typeof formatLabels, string>> = { bold: "B", italic: "I", link: "K" };

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

function writeDraft(key: string, value: string): boolean {
  try {
    if (new TextEncoder().encode(value).byteLength > MAX_DRAFT_BYTES) {
      return false;
    }
    if (value === "") window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}
