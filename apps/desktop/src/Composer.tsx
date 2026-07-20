import { useLayoutEffect, useState, type CSSProperties, type RefObject } from "react";

import type { PermissionMode, RunContext } from "./types";
import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { IconButton, Select, TextArea, Tooltip } from "./ui/primitives";

const MAX_DRAFT_BYTES = 256 * 1024;
const MAX_COMPOSER_HEIGHT = 176;

export function Composer({
  draftKey,
  active,
  submitting,
  controlBusy,
  composerRef,
  runContext,
  runContextBusy,
  modelChanging,
  permissionMode,
  onModelChange,
  onPermissionModeChange,
  onSubmit,
  onCancel,
}: {
  draftKey: string;
  active: boolean;
  submitting: boolean;
  controlBusy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  runContext?: RunContext;
  runContextBusy: boolean;
  modelChanging: boolean;
  permissionMode: PermissionMode;
  onModelChange: (modelName: string) => void;
  onPermissionModeChange: (mode: PermissionMode) => void;
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => void;
}) {
  const { t } = useLocale();
  const [prompt, setPrompt] = useState(() => readDraft(draftKey));
  useLayoutEffect(() => {
    const input = composerRef.current;
    if (input === null) return;
    input.style.height = "0px";
    const nextHeight = Math.min(input.scrollHeight, MAX_COMPOSER_HEIGHT);
    input.style.height = `${nextHeight}px`;
    input.style.overflowY = input.scrollHeight > MAX_COMPOSER_HEIGHT ? "auto" : "hidden";
  }, [composerRef, prompt]);

  const submit = async () => {
    const nextPrompt = prompt.trim();
    if (nextPrompt === "" || active || submitting) return;
    if (await onSubmit(nextPrompt)) {
      setPrompt("");
      writeDraft(draftKey, "");
    }
  };
  const modelName = runContext?.modelName ?? (runContextBusy ? t("loadingModel") : t("modelUnavailable"));
  const models = runContext?.availableModels ?? (runContext === undefined ? [] : [runContext.modelName]);
  const permissionModes = runContext?.availablePermissionModes ?? ["read-only", "manual", "auto-edit", "danger-full-access"];

  return (
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <div className="composer-surface">
        <TextArea
          id="desktop-prompt"
          className="composer-input"
          containerClassName="composer-input-field"
          label={t("messageSigil")}
          labelHidden
          ref={composerRef}
          value={prompt}
          onChange={(event) => {
            setPrompt(event.target.value);
            writeDraft(draftKey, event.target.value);
          }}
          placeholder={active ? t("activePrompt") : t("prompt")}
          rows={1}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
            event.preventDefault();
            void submit();
          }}
        />
        <div className="composer-toolbar">
          <div className="composer-options">
            <Tooltip label={runContext === undefined ? modelName : t("modelHint", { provider: runContext.providerName })}>
              <div className="composer-model">
                <Icon name="model" />
                <Select
                  label={t("model")}
                  labelHidden
                  containerClassName="composer-model-field"
                  className="composer-model-select"
                  value={runContext?.modelName ?? ""}
                  disabled={active || runContextBusy || modelChanging || models.length < 2}
                  onChange={(event) => onModelChange(event.target.value)}
                >
                  {models.length === 0 ? <option value="">{modelName}</option> : models.map((model) => (
                    <option key={model} value={model}>{model}</option>
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
                disabled={active || runContextBusy}
                onChange={(event) => onPermissionModeChange(event.target.value as PermissionMode)}
              >
                {permissionModes.map((mode) => (
                  <option key={mode} value={mode}>{permissionModeLabel(mode, t)}</option>
                ))}
              </Select>
            </div>
            <ContextUsage context={runContext} loading={runContextBusy} />
          </div>
          {active ? (
            <Tooltip label={t("stopRunHint")}>
              <IconButton
                className="composer-submit composer-stop"
                type="button"
                aria-label={t("stopRun")}
                icon={<Icon name="stop" />}
                disabled={controlBusy}
                onClick={onCancel}
              />
            </Tooltip>
          ) : (
            <Tooltip label={submitting ? t("startingRun") : t("sendMessageHint")}>
              <IconButton
                className="composer-submit sg-icon-button-primary"
                type="submit"
                aria-label={t("sendMessage")}
                icon={<Icon name="send" />}
                disabled={prompt.trim() === "" || submitting}
                aria-busy={submitting || undefined}
              />
            </Tooltip>
          )}
        </div>
      </div>
    </form>
  );
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
