import { useLayoutEffect, useState, type CSSProperties, type RefObject } from "react";

import type { RunApprovalMode, RunContext } from "./types";
import { Icon } from "./ui/icons";
import { Button, IconButton, Select, TextArea, Tooltip } from "./ui/primitives";

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
  approvalMode,
  onApprovalModeChange,
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
  approvalMode: RunApprovalMode;
  onApprovalModeChange: (mode: RunApprovalMode) => void;
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => void;
}) {
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
  const modelName = runContext?.modelName ?? (runContextBusy ? "Loading model…" : "Model unavailable");
  const approvalModes = runContext?.availableApprovalModes ?? ["ask", "allow_readonly", "deny"];

  return (
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <div className="composer-surface">
        <TextArea
          id="desktop-prompt"
          className="composer-input"
          containerClassName="composer-input-field"
          label="Message Sigil"
          labelHidden
          ref={composerRef}
          value={prompt}
          onChange={(event) => {
            setPrompt(event.target.value);
            writeDraft(draftKey, event.target.value);
          }}
          placeholder={active ? "Draft the next message while this run finishes…" : "Ask Sigil to inspect, explain, or change code…"}
          rows={1}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
            event.preventDefault();
            void submit();
          }}
        />
        <div className="composer-toolbar">
          <div className="composer-options">
            <Tooltip label={runContext === undefined ? modelName : `${runContext.providerName} · fixed for this conversation`}>
              <Button className="composer-chip composer-model" variant="quiet" type="button" leadingIcon={<Icon name="lock" />} disabled>
                {modelName}
              </Button>
            </Tooltip>
            <div className="composer-mode">
              <Icon name="shield" />
              <Select
                label="Approval mode"
                labelHidden
                containerClassName="composer-mode-field"
                className="composer-mode-select"
                value={approvalMode}
                disabled={active || runContextBusy}
                onChange={(event) => onApprovalModeChange(event.target.value as RunApprovalMode)}
              >
                {approvalModes.map((mode) => (
                  <option key={mode} value={mode}>{approvalModeLabel(mode)}</option>
                ))}
              </Select>
            </div>
            <ContextUsage context={runContext} loading={runContextBusy} />
          </div>
          {active ? (
            <Tooltip label="Stop this run cooperatively">
              <IconButton
                className="composer-submit composer-stop"
                type="button"
                aria-label="Stop run"
                icon={<Icon name="stop" />}
                disabled={controlBusy}
                onClick={onCancel}
              />
            </Tooltip>
          ) : (
            <Tooltip label={submitting ? "Starting run" : "Send message (Enter)"}>
              <IconButton
                className="composer-submit sg-icon-button-primary"
                type="submit"
                aria-label="Send message"
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
  const used = context?.lastPromptTokens;
  const limit = context?.contextWindowTokens;
  if (loading) {
    return <span className="context-usage context-unavailable" aria-label="Loading context usage">Context…</span>;
  }
  if (used === undefined || limit === undefined || limit === 0) {
    return <span className="context-usage context-unavailable" aria-label="Context usage unavailable">Context —</span>;
  }
  const boundedUsed = Math.min(used, limit);
  const ratio = boundedUsed / limit;
  const percent = ratio * 100;
  const percentLabel = percent < 1 ? percent.toFixed(1) : Math.round(percent).toString();
  const style = { "--context-used": `${ratio * 100}%` } as CSSProperties;
  return (
    <Tooltip label={`${formatTokens(used)} of ${formatTokens(limit)} context tokens used`}>
      <span
        className="context-usage"
        role="meter"
        aria-label={`Context usage ${percentLabel}%`}
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

function approvalModeLabel(mode: RunApprovalMode): string {
  switch (mode) {
    case "ask": return "Ask";
    case "allow_readonly": return "Read only";
    case "deny": return "No tools";
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
