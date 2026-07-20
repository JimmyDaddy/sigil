import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { ApprovalDock } from "./ApprovalDock";
import type { DesktopBridge } from "./bridge";
import { Composer, draftStorageKey } from "./Composer";
import { ErrorCard } from "./ErrorCard";
import { ExtensionWorkbench } from "./ExtensionWorkbench";
import { translateEnglish, useLocale, type Translate } from "./i18n";
import { Message, type MessageView } from "./Message";
import { ToolCard } from "./ToolCard";
import type {
  AgentBinding,
  AgentCatalogEntry,
  PermissionMode,
  ReasoningEffort,
  RunContext,
  RunStreamStatus,
  RunSummary,
  SessionSummary,
  SkillBinding,
  SkillCatalogEntry,
  TimelineEvent,
  TranscriptMessage,
  VerificationSummary,
} from "./types";
import { Icon } from "./ui/icons";
import { Button, Drawer, IconButton, Tooltip } from "./ui/primitives";
import { VerificationInspector } from "./VerificationInspector";

interface ConversationPanelProps {
  bridge: DesktopBridge;
  workspaceId: string;
  session: SessionSummary;
  onNewSession: () => Promise<boolean>;
  onOpenSessionPicker: (query: string) => void;
}

interface TimelineRowBase {
  key: string;
  label: string;
  text: string;
  status?: string;
}

type TimelineRow =
  | (TimelineRowBase & { kind: MessageView["kind"] })
  | (TimelineRowBase & { kind: "tool" });

export function ConversationPanel({
  bridge,
  workspaceId,
  session,
  onNewSession,
  onOpenSessionPicker,
}: ConversationPanelProps) {
  const { t } = useLocale();
  const [run, setRun] = useState<RunSummary>();
  const [runContext, setRunContext] = useState<RunContext>();
  const [runContextBusy, setRunContextBusy] = useState(false);
  const [runContextReload, setRunContextReload] = useState(0);
  const [permissionMode, setPermissionMode] = useState<PermissionMode>("manual");
  const [reasoningEffort, setReasoningEffort] = useState<ReasoningEffort>();
  const [selectedModelName, setSelectedModelName] = useState<string>();
  const [events, setEvents] = useState<TimelineEvent[]>([]);
  const [streamStatus, setStreamStatus] = useState<RunStreamStatus>();
  const [submitting, setSubmitting] = useState(false);
  const [controlBusy, setControlBusy] = useState(false);
  const [verification, setVerification] = useState<VerificationSummary>();
  const [verificationBusy, setVerificationBusy] = useState(false);
  const [transcript, setTranscript] = useState<TranscriptMessage[]>([]);
  const [transcriptTotal, setTranscriptTotal] = useState(0);
  const [nextBefore, setNextBefore] = useState<number>();
  const [transcriptBusy, setTranscriptBusy] = useState(false);
  const [transcriptError, setTranscriptError] = useState(false);
  const [transcriptReload, setTranscriptReload] = useState(0);
  const [attachmentGap, setAttachmentGap] = useState(false);
  const [runNotice, setRunNotice] = useState<{ message: string; error: boolean }>();
  const [runAnnouncement, setRunAnnouncement] = useState("");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [extensionWorkbenchOpen, setExtensionWorkbenchOpen] = useState(false);
  const [extensionWorkbenchKind, setExtensionWorkbenchKind] = useState<"skills" | "agents">("skills");
  const [extensionWorkbenchQuery, setExtensionWorkbenchQuery] = useState("");
  const [requestedSkill, setRequestedSkill] = useState<SkillCatalogEntry>();
  const [requestedAgent, setRequestedAgent] = useState<AgentCatalogEntry>();
  const timelineRef = useRef<HTMLDivElement>(null);
  const timelinePinnedToEnd = useRef(true);
  const prependScrollHeight = useRef<number | undefined>(undefined);
  const activeRunIdRef = useRef<string | undefined>(undefined);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const inspectorTriggerRef = useRef<HTMLButtonElement>(null);
  const extensionTriggerRef = useRef<HTMLButtonElement>(null);
  const onNotice = useCallback((message: string, error = false) => {
    setRunNotice({ message, error });
  }, []);
  useEffect(() => {
    setRun(undefined);
    setRunContext(undefined);
    setRunContextBusy(false);
    setPermissionMode("manual");
    setReasoningEffort(undefined);
    setSelectedModelName(undefined);
    setEvents([]);
    setStreamStatus(undefined);
    setVerification(undefined);
    setTranscript([]);
    setTranscriptTotal(0);
    setNextBefore(undefined);
    setTranscriptError(false);
    setAttachmentGap(false);
    setRunNotice(undefined);
    setRunAnnouncement("");
    setInspectorOpen(false);
    setExtensionWorkbenchOpen(false);
    setExtensionWorkbenchKind("skills");
    setExtensionWorkbenchQuery("");
    setRequestedSkill(undefined);
    setRequestedAgent(undefined);
    activeRunIdRef.current = undefined;
  }, [session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    setRunContextBusy(true);
    void bridge
      .runContext(workspaceId, session.id)
      .then((context) => {
        if (disposed) return;
        setRunContext(context);
        setSelectedModelName(context.modelName);
        setPermissionMode((current) =>
          activeRunIdRef.current === undefined ? context.defaultPermissionMode : current,
        );
        setReasoningEffort((current) =>
          current !== undefined && context.availableReasoningEfforts.includes(current)
            ? current
            : context.defaultReasoningEffort,
        );
      })
      .catch(() => {
        if (!disposed) setRunContext(undefined);
      })
      .finally(() => {
        if (!disposed) setRunContextBusy(false);
      });
    return () => {
      disposed = true;
    };
  }, [bridge, runContextReload, session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    setTranscriptBusy(true);
    void bridge
      .transcript(workspaceId, session.id, { limit: 50 })
      .then((page) => {
        if (disposed) return;
        setTranscript(page.messages);
        setTranscriptTotal(page.totalMessages);
        setNextBefore(page.nextBefore);
        setTranscriptError(false);
      })
      .catch(() => {
        if (!disposed) setTranscriptError(true);
      })
      .finally(() => {
        if (!disposed) setTranscriptBusy(false);
      });
    return () => {
      disposed = true;
    };
  }, [bridge, session.id, transcriptReload, workspaceId]);

  useEffect(() => {
    let disposed = false;
    void bridge.verification(workspaceId, session.id).then((view) => {
      if (!disposed) setVerification(view);
    }).catch(() => {
      if (!disposed) setVerification(undefined);
    });
    return () => {
      disposed = true;
    };
  }, [bridge, session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    const unsubscribers: Array<() => void> = [];
    const setup = async () => {
      const unsubscribeEvents = await bridge.subscribeRunEvents((event) => {
        if (
          disposed ||
          event.workspaceId !== workspaceId ||
          event.sessionId !== session.id
        ) {
          return;
        }
        if (activeRunIdRef.current === undefined) activeRunIdRef.current = event.runId;
        if (activeRunIdRef.current !== event.runId) return;
        setEvents((current) => mergeTimelineEvent(current, event));
        const terminalStatus = terminalStatusForEvent(event);
        if (terminalStatus !== undefined) {
          setRun((current) =>
            current?.id === event.runId ? { ...current, status: terminalStatus } : current,
          );
        }
      });
      if (disposed) {
        unsubscribeEvents();
        return;
      }
      unsubscribers.push(unsubscribeEvents);

      const unsubscribeStatus = await bridge.subscribeRunStreamStatus((status) => {
        if (
          disposed ||
          status.workspaceId !== workspaceId ||
          status.sessionId !== session.id
        ) {
          return;
        }
        if (activeRunIdRef.current === undefined) activeRunIdRef.current = status.runId;
        if (activeRunIdRef.current !== status.runId) return;
        setStreamStatus(status);
        if (status.message !== undefined) onNotice(status.message, status.state === "error");
        if (status.state === "terminal") {
          setRunAnnouncement(status.message ?? t("runFinishedAnnouncement"));
          setRunContextReload((value) => value + 1);
          void bridge.verification(workspaceId, session.id).then(setVerification).catch(() => {
            setVerification(undefined);
          });
        }
      });
      if (disposed) {
        unsubscribeStatus();
        return;
      }
      unsubscribers.push(unsubscribeStatus);

      if (session.foregroundRunId === undefined) return;
      activeRunIdRef.current = session.foregroundRunId;
      try {
        const attachment = await bridge.attachRun(
          workspaceId,
          session.id,
          session.foregroundRunId,
        );
        if (disposed) return;
        setRun(attachment.run);
        setPermissionMode(attachment.run.permissionMode);
        setReasoningEffort(attachment.run.reasoningEffort);
        setEvents((current) =>
          attachment.events.reduce(mergeTimelineEvent, current),
        );
        setStreamStatus({
          workspaceId,
          sessionId: session.id,
          runId: attachment.run.id,
          state: attachment.streamState,
          message: attachment.streamMessage,
        });
        setAttachmentGap(attachment.hasGap);
        if (attachment.streamMessage !== undefined) {
          onNotice(
            attachment.streamMessage,
            attachment.streamState === "error",
          );
        }
      } catch {
        if (!disposed) {
          activeRunIdRef.current = undefined;
          onNotice(
            t("activeRunChanged"),
            true,
          );
        }
      }
    };
    void setup().catch(() => {
      if (!disposed) {
        onNotice(t("liveControlsUnavailable"), true);
      }
    });
    return () => {
      disposed = true;
      for (const unsubscribe of unsubscribers) unsubscribe();
    };
  }, [bridge, onNotice, session.foregroundRunId, session.id, t, workspaceId]);

  const rows = useMemo(
    () => [...reduceTranscript(transcript, t), ...reduceTimeline(events, t)],
    [events, t, transcript],
  );
  const pendingApproval = useMemo(() => latestPendingApproval(events), [events]);
  const active = run !== undefined && !isTerminal(run.status) && streamStatus?.state !== "terminal";

  useEffect(() => {
    if (pendingApproval?.approval !== undefined) setInspectorOpen(false);
  }, [pendingApproval?.approval]);

  useLayoutEffect(() => {
    const timeline = timelineRef.current;
    if (timeline === null) return;
    if (prependScrollHeight.current !== undefined) {
      timeline.scrollTop += timeline.scrollHeight - prependScrollHeight.current;
      prependScrollHeight.current = undefined;
    } else if (timelinePinnedToEnd.current) {
      timeline.scrollTop = timeline.scrollHeight;
    }
  }, [rows.length]);

  const loadEarlier = async () => {
    if (nextBefore === undefined || transcriptBusy) return;
    const timeline = timelineRef.current;
    if (timeline !== null) prependScrollHeight.current = timeline.scrollHeight;
    setTranscriptBusy(true);
    try {
      const page = await bridge.transcript(workspaceId, session.id, {
        before: nextBefore,
        limit: 50,
      });
      setTranscript((current) => {
        const merged = mergeTranscriptPage(page.messages, current);
        if (merged.length === current.length) prependScrollHeight.current = undefined;
        return merged;
      });
      setTranscriptTotal(page.totalMessages);
      setNextBefore(page.nextBefore);
      setTranscriptError(false);
    } catch {
      prependScrollHeight.current = undefined;
      setTranscriptError(true);
    } finally {
      setTranscriptBusy(false);
    }
  };

  const submit = async (
    nextPrompt: string,
    skillBinding?: SkillBinding,
    agentBinding?: AgentBinding,
  ): Promise<boolean> => {
    if (nextPrompt === "" || active || submitting) return false;
    setSubmitting(true);
    onNotice(t("startingRunNotice"));
    try {
      const modelChanged =
        runContext !== undefined && selectedModelName !== runContext.modelName;
      const selectedModelOption = runContext?.modelOptions.find(
        (option) => option.modelName === selectedModelName,
      );
      const selectedReasoningEffort = reasoningEffort !== undefined &&
        selectedModelOption?.availableReasoningEfforts.includes(reasoningEffort)
        ? reasoningEffort
        : undefined;
      const started = await bridge.startRun(
        workspaceId,
        session.id,
        nextPrompt,
        permissionMode,
        modelChanged ? selectedModelName : undefined,
        modelChanged ? runContext?.modelSelectionBinding : undefined,
        selectedReasoningEffort,
        selectedReasoningEffort === undefined
          ? undefined
          : selectedModelOption?.reasoningEffortBinding,
        skillBinding,
        agentBinding,
      );
      activeRunIdRef.current = started.id;
      setRun(started);
      setPermissionMode(started.permissionMode);
      setReasoningEffort(started.reasoningEffort ?? selectedReasoningEffort);
      if (modelChanged) setRunContextReload((current) => current + 1);
      onNotice(t("runStarted"));
      return true;
    } catch {
      onNotice(t("runStartFailed"), true);
      return false;
    } finally {
      setSubmitting(false);
    }
  };

  const cancel = async () => {
    if (run === undefined || !active || controlBusy) return;
    setControlBusy(true);
    onNotice(t("requestingCancellation"));
    try {
      setRun(await bridge.cancelRun(workspaceId, session.id, run.id));
      onNotice(t("cancellationRequested"));
    } catch {
      onNotice(t("cancellationFailed"), true);
    } finally {
      setControlBusy(false);
    }
  };

  const decideApproval = async (approve: boolean) => {
    if (pendingApproval?.approval === undefined || controlBusy) return;
    setControlBusy(true);
    onNotice(approve ? t("submittingApproval") : t("submittingDenial"));
    try {
      const decision = await bridge.resolveApproval(
        workspaceId,
        session.id,
        pendingApproval.runId,
        pendingApproval.approval,
        approve,
      );
      onNotice(t("toolRequestDecision", { decision: decision.decision }));
    } catch {
      onNotice(t("approvalDecisionFailed"), true);
    } finally {
      setControlBusy(false);
    }
  };

  const rerunVerification = async () => {
    if (verification?.action?.kind !== "rerun" || verificationBusy || active) return;
    setVerificationBusy(true);
    onNotice(t("runningRecommendedCheck", { check: verification.recommendedCheckSpecId ?? "" }));
    try {
      const next = await bridge.rerunVerification(
        workspaceId,
        session.id,
        verification.action.request,
      );
      setVerification(next);
      onNotice(t("verificationFinished", { status: next.status }));
    } catch {
      onNotice(t("verificationChanged"), true);
      try {
        setVerification(await bridge.verification(workspaceId, session.id));
      } catch {
        setVerification(undefined);
      }
    } finally {
      setVerificationBusy(false);
    }
  };

  return (
    <div className="conversation-layout">
      <section className="conversation-panel" aria-labelledby="conversation-title">
      <header className="conversation-header">
        <div>
          <span className="conversation-title-row">
            <h2 id="conversation-title">{session.label ?? t("untitledConversation")}</h2>
            <small>{t("recordedRuns", { count: session.runCount })}</small>
          </span>
        </div>
        <div className="conversation-header-actions">
          <span className={`stream-chip stream-${streamStatus?.state ?? "idle"}`}>
            {streamStatus?.state ?? "ready"}
          </span>
          <Tooltip label={t("openExtensions")}>
            <IconButton
              className="extension-trigger"
              ref={extensionTriggerRef}
              type="button"
              aria-label={t("openExtensions")}
              aria-controls="extension-workbench"
              aria-expanded={extensionWorkbenchOpen}
              icon={<Icon name="extensions" />}
              onClick={() => {
                setExtensionWorkbenchKind("skills");
                setExtensionWorkbenchQuery("");
                setExtensionWorkbenchOpen(true);
              }}
            />
          </Tooltip>
          {verification !== undefined ? (
            <Tooltip
              label={pendingApproval?.approval !== undefined
                ? t("resolveApprovalFirst")
                : t("verificationStatus", { status: verification.status })}
            >
              <IconButton
                className={`review-trigger review-${verification.verdict}`}
                ref={inspectorTriggerRef}
                type="button"
                aria-label={t("openVerification", { status: verification.status })}
                aria-controls="verification-inspector"
                aria-expanded={inspectorOpen}
                disabled={pendingApproval?.approval !== undefined}
                icon={<Icon name={verification.verdict === "passed" ? "check" : "warning"} />}
                onClick={() => setInspectorOpen(true)}
              />
            </Tooltip>
          ) : null}
        </div>
      </header>

      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">{runAnnouncement}</div>

      {runNotice !== undefined ? (
        runNotice.error
          ? <ErrorCard title={t("runActionAttention")} message={runNotice.message} actionLabel={t("dismiss")} onAction={() => setRunNotice(undefined)} />
          : <div className="run-notice" role="status">{runNotice.message}</div>
      ) : null}

      <div
        className="timeline"
        ref={timelineRef}
        role="log"
        aria-live="off"
        aria-label={t("conversationTimeline")}
        onScroll={(event) => {
          const timeline = event.currentTarget;
          timelinePinnedToEnd.current =
            timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight <= 48;
        }}
      >
        {attachmentGap ? (
          <div className="timeline-gap" role="status">
            {t("liveDetailGap")}
          </div>
        ) : null}
        {nextBefore !== undefined ? (
          <div className="transcript-pagination">
            <Button
              variant="quiet"
              type="button"
              disabled={transcriptBusy}
              onClick={() => void loadEarlier()}
            >
              {transcriptBusy
                ? t("loadingEarlierMessages")
                : t("loadEarlierMessages", { count: Math.max(0, transcriptTotal - transcript.length) })}
            </Button>
          </div>
        ) : null}
        {transcriptError ? (
          <ErrorCard
            title={t("savedMessagesUnavailable")}
            message={t("savedMessagesRetryDetail")}
            actionLabel={transcriptBusy ? t("retrying") : t("retryMessages")}
            actionDisabled={transcriptBusy}
            onAction={() => setTranscriptReload((value) => value + 1)}
          />
        ) : null}
        {rows.length === 0 ? (
          <div className="timeline-empty">
            <strong>{transcriptBusy ? t("loadingConversationHistory") : t("readyForPrompt")}</strong>
            <span>{transcriptBusy ? t("loadingSavedMessages") : t("newRunActivity")}</span>
          </div>
        ) : (
          rows.map((row) => row.kind === "tool"
            ? <ToolCard key={row.key} tool={{ key: row.key, toolName: row.label, text: row.text, status: row.status }} />
            : <Message key={row.key} message={row} onOpenExternalUrl={bridge.openExternalUrl} />)
        )}
      </div>

      {pendingApproval?.approval !== undefined ? (
        <ApprovalDock
          approval={pendingApproval.approval}
          busy={controlBusy}
          composerRef={composerRef}
          onDecision={(approve) => void decideApproval(approve)}
        />
      ) : null}

      <Composer
        key={draftStorageKey(workspaceId, session.id)}
        draftKey={draftStorageKey(workspaceId, session.id)}
        active={active}
        submitting={submitting}
        controlBusy={controlBusy}
        composerRef={composerRef}
        runContext={runContext}
        runContextBusy={runContextBusy}
        selectedModelName={selectedModelName}
        permissionMode={permissionMode}
        reasoningEffort={reasoningEffort}
        requestedSkill={requestedSkill}
        requestedAgent={requestedAgent}
        onModelChange={(modelName) => {
          setSelectedModelName(modelName);
          const modelOption = runContext?.modelOptions.find(
            (option) => option.modelName === modelName,
          );
          setReasoningEffort((current) =>
            current !== undefined && modelOption?.availableReasoningEfforts.includes(current)
              ? current
              : modelOption?.defaultReasoningEffort,
          );
        }}
        onPermissionModeChange={setPermissionMode}
        onReasoningEffortChange={setReasoningEffort}
        onNewSession={onNewSession}
        onOpenSessionPicker={onOpenSessionPicker}
        onOpenAgentWorkbench={(query) => {
          setExtensionWorkbenchKind("agents");
          setExtensionWorkbenchQuery(query);
          setExtensionWorkbenchOpen(true);
        }}
        onNotice={onNotice}
        onSubmit={submit}
        onCancel={() => void cancel()}
      />
      </section>

      {verification !== undefined ? (
        <Drawer
          id="verification-inspector"
          open={inspectorOpen}
          title={t("verification")}
          description={t("verificationDetail")}
          returnFocusRef={inspectorTriggerRef}
          onOpenChange={setInspectorOpen}
        >
          <VerificationInspector verification={verification} busy={verificationBusy} runActive={active} onRerun={() => void rerunVerification()} />
        </Drawer>
      ) : null}
      <Drawer
        id="extension-workbench"
        open={extensionWorkbenchOpen}
        title={t("extensions")}
        description={t("extensionsDetail")}
        returnFocusRef={extensionTriggerRef}
        onOpenChange={setExtensionWorkbenchOpen}
      >
        {runContext === undefined ? (
          <div className="extension-detail extension-empty">{t("extensionsUnavailable")}</div>
        ) : (
          <ExtensionWorkbench
            catalog={runContext.extensionCatalog}
            runActive={active}
            initialKind={extensionWorkbenchKind}
            initialQuery={extensionWorkbenchQuery}
            onUseSkill={(skill) => {
              setRequestedSkill({ ...skill });
              setRequestedAgent(undefined);
              setExtensionWorkbenchOpen(false);
            }}
            onUseAgent={(agent) => {
              setRequestedAgent({ ...agent });
              setRequestedSkill(undefined);
              setExtensionWorkbenchOpen(false);
            }}
          />
        )}
      </Drawer>
    </div>
  );
}

function latestPendingApproval(events: TimelineEvent[]): TimelineEvent | undefined {
  const pending = new Map<string, TimelineEvent>();
  for (const event of events) {
    if (event.kind === "approval_requested" && event.itemId !== undefined) {
      pending.set(`${event.runId}:${event.itemId}`, event);
    }
    if (event.kind === "approval_resolved" && event.itemId !== undefined) {
      pending.delete(`${event.runId}:${event.itemId}`);
    }
  }
  const pendingEvents = [...pending.values()];
  return pendingEvents[pendingEvents.length - 1];
}

export function mergeTranscriptPage(
  older: TranscriptMessage[],
  current: TranscriptMessage[],
): TranscriptMessage[] {
  const messages = new Map<string, TranscriptMessage>();
  for (const message of [...older, ...current]) {
    messages.set(message.messageId, message);
  }
  return [...messages.values()].sort((left, right) => left.ordinal - right.ordinal);
}

export function reduceTranscript(
  messages: TranscriptMessage[],
  t: Translate = translateEnglish,
): TimelineRow[] {
  return messages.map((message) => {
    const attachmentText = message.imageAttachmentCount > 0
      ? `${message.imageAttachmentCount} image attachment${message.imageAttachmentCount === 1 ? "" : "s"} recorded.`
      : "";
    const text = (message.content ?? attachmentText) || "";
    const status = message.truncated
      ? `preview · ${message.originalContentBytes} bytes`
      : message.imageAttachmentCount > 0
        ? `${message.imageAttachmentCount} attachment${message.imageAttachmentCount === 1 ? "" : "s"}`
        : undefined;
    if (message.role === "user") {
      return {
        key: `history:${message.messageId}`,
        kind: "user",
        label: t("you"),
        text,
        status,
      };
    }
    if (message.role === "tool") {
      return {
        key: `history:${message.messageId}`,
        kind: "tool",
        label: message.toolName ?? t("toolResult"),
        text,
        status,
      };
    }
    if (message.assistantKind === "reasoning_trace") {
      return {
        key: `history:${message.messageId}`,
        kind: "reasoning",
        label: t("reasoning"),
        text,
        status,
      };
    }
    if (message.assistantKind === "progress") {
      return {
        key: `history:${message.messageId}`,
        kind: "progress",
        label: t("progress"),
        text,
        status,
      };
    }
    return {
      key: `history:${message.messageId}`,
      kind: "assistant",
      label: "Sigil",
      text,
      status: status ?? (message.assistantKind === "tool_preamble" ? "tool preamble" : undefined),
    };
  });
}

export function mergeTimelineEvent(
  current: TimelineEvent[],
  incoming: TimelineEvent,
): TimelineEvent[] {
  const key = eventKey(incoming);
  if (current.some((event) => eventKey(event) === key)) return current;
  const laterInSameRun = current.findIndex(
    (event) => event.runId === incoming.runId && event.sequence > incoming.sequence,
  );
  if (laterInSameRun === -1) return [...current, incoming];
  return [
    ...current.slice(0, laterInSameRun),
    incoming,
    ...current.slice(laterInSameRun),
  ];
}

export function reduceTimeline(
  events: TimelineEvent[],
  t: Translate = translateEnglish,
): TimelineRow[] {
  const rows = new Map<string, TimelineRow>();
  for (const event of events) {
    const assistantKey = `${event.runId}:assistant`;
    switch (event.kind) {
      case "run_started":
        rows.set(`${event.runId}:user`, {
          key: `${event.runId}:user`, kind: "user", label: t("you"), text: event.text ?? "",
        });
        break;
      case "assistant_delta": {
        const current = rows.get(assistantKey);
        rows.set(assistantKey, {
          key: assistantKey,
          kind: "assistant",
          label: "Sigil",
          text: `${current?.text ?? ""}${event.text ?? ""}`,
        });
        break;
      }
      case "assistant_message":
        rows.set(assistantKey, {
          key: assistantKey,
          kind: "assistant",
          label: "Sigil",
          text: event.text ?? rows.get(assistantKey)?.text ?? "",
        });
        break;
      case "run_finished": {
        finalizeRunRows(rows, event.runId, t);
        const current = rows.get(assistantKey);
        if (current === undefined || current.text === "") {
          rows.set(assistantKey, {
            key: assistantKey,
            kind: "assistant",
            label: "Sigil",
            text: event.text ?? t("runCompleted"),
          });
        }
        break;
      }
      case "reasoning_delta": {
        const key = `${event.runId}:reasoning`;
        const current = rows.get(key);
        rows.set(key, {
          key,
          kind: "reasoning",
          label: t("working"),
          text: `${current?.text ?? ""}${event.text ?? ""}`,
        });
        break;
      }
      case "tool_started":
      case "tool_completed":
      case "tool_progress":
      case "tool_result": {
        const key = `${event.runId}:tool:${event.itemId ?? event.sequence}`;
        const current = rows.get(key);
        rows.set(key, {
          key,
          kind: "tool",
          label: event.toolName ?? current?.label ?? t("tool"),
          text: event.text ?? current?.text ?? "",
          status: event.status ?? event.kind.replace("tool_", ""),
        });
        break;
      }
      case "approval_requested": {
        const key = `${event.runId}:approval:${event.itemId ?? event.sequence}`;
        rows.set(key, {
          key,
          kind: "notice",
          label: t("approvalRequired"),
          text: t("toolWaitingDecision", { tool: event.toolName ?? t("tool") }),
          status: "waiting",
        });
        break;
      }
      case "run_failed":
      case "run_cancelled":
        finalizeRunRows(rows, event.runId, t);
        rows.set(`${event.runId}:terminal`, {
          key: `${event.runId}:terminal`,
          kind: "error",
          label: event.kind === "run_cancelled" ? t("cancelled") : t("runFailed"),
          text: event.text ?? (event.kind === "run_cancelled" ? t("runCancelled") : t("runFailedDetail")),
        });
        break;
      case "notice":
        rows.set(`${event.runId}:notice:${event.sequence}`, {
          key: `${event.runId}:notice:${event.sequence}`,
          kind: "notice",
          label: t("notice"),
          text: event.text ?? t("runNotice"),
        });
        break;
      default:
        break;
    }
  }
  return [...rows.values()];
}

function finalizeRunRows(
  rows: Map<string, TimelineRow>,
  runId: string,
  t: Translate,
) {
  const reasoningKey = `${runId}:reasoning`;
  const reasoning = rows.get(reasoningKey);
  if (reasoning !== undefined) {
    rows.set(reasoningKey, { ...reasoning, label: t("reasoning"), status: undefined });
  }
  const progressKey = `${runId}:progress`;
  const progress = rows.get(progressKey);
  if (progress !== undefined) {
    rows.set(progressKey, { ...progress, label: t("progress"), status: undefined });
  }
}

function eventKey(event: TimelineEvent): string {
  return `${event.runId}:${event.sequence}:${event.kind}`;
}

function isTerminal(status: RunSummary["status"]): boolean {
  return ["finished", "failed", "cancelled", "interrupted"].includes(status);
}

function terminalStatusForEvent(event: TimelineEvent): RunSummary["status"] | undefined {
  if (event.kind === "run_finished") return "finished";
  if (event.kind === "run_failed") return "failed";
  if (event.kind === "run_cancelled") return "cancelled";
  return undefined;
}
