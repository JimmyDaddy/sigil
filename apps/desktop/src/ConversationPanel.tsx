import { useCallback, useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState } from "react";

import { ApprovalDock } from "./ApprovalDock";
import { AgentActivityPanel } from "./AgentActivityPanel";
import type { DesktopBridge } from "./bridge";
import { Composer, draftStorageKey } from "./Composer";
import { ErrorCard } from "./ErrorCard";
import { ExtensionWorkbench } from "./ExtensionWorkbench";
import { useLocale, type Translate } from "./i18n";
import { Message } from "./Message";
import { ToolCard } from "./ToolCard";
import type {
  AgentActivitySummary,
  AgentBinding,
  AgentCatalogEntry,
  ApprovalAction,
  ContinuityRecoveryAction,
  PermissionMode,
  ReasoningEffort,
  RunContext,
  RunStreamState,
  RunStreamStatus,
  RunSummary,
  SessionSummary,
  SkillBinding,
  SkillCatalogEntry,
  TimelineEvent,
  VerificationSummary,
} from "./types";
import type { ConversationDisplayPage as BridgeConversationDisplayPage } from "./types";
import {
  createConversationContinuityState,
  reduceConversationContinuity,
  resolveConversationIdentity,
  selectConversationTimeline,
  type ConversationContinuityState,
  type ConversationDisplayPage,
  type ConversationTerminalObservation,
} from "./features/conversation/continuityReducer";
import { projectConversationRows } from "./features/conversation/conversationRows";
import {
  createLiveEventState,
  liveEventReducer,
  selectDeltaBuffers,
  selectLatestPendingApproval,
  semanticLiveItemFromTimelineEvent,
  terminalSignalFromTimelineEvent,
} from "./features/conversation/liveEventReducer";
import { Icon } from "./ui/icons";
import { Button, Drawer, IconButton, Tooltip } from "./ui/primitives";
import { useNotifications } from "./ui/feedback";
import { VerificationInspector } from "./VerificationInspector";

interface ConversationPanelProps {
  bridge: DesktopBridge;
  workspaceId: string;
  session: SessionSummary;
  onInitialLoadComplete?: (sessionId: string) => void;
  onRunContextChange?: (context: RunContext) => void;
  onNewSession: () => Promise<boolean>;
  onOpenWorkspacePicker: () => void;
  onOpenSessionPicker: (query: string) => void;
  onOpenSettings: () => void;
  onOpenSupport: () => void;
}

interface PendingPrompt {
  readonly identity: string;
  readonly text: string;
  readonly runId?: string;
}

interface TimelineAnchor {
  readonly displayId: string;
  readonly viewportOffset: number;
}

export function ConversationPanel({
  bridge,
  workspaceId,
  session,
  onInitialLoadComplete,
  onRunContextChange,
  onNewSession,
  onOpenWorkspacePicker,
  onOpenSessionPicker,
  onOpenSettings,
  onOpenSupport,
}: ConversationPanelProps) {
  const { t } = useLocale();
  const { notify } = useNotifications();
  const [run, setRun] = useState<RunSummary>();
  const [agentActivity, setAgentActivity] = useState<AgentActivitySummary>();
  const [agentActivityBusy, setAgentActivityBusy] = useState(false);
  const [agentActivityError, setAgentActivityError] = useState(false);
  const [agentActivityReload, setAgentActivityReload] = useState(0);
  const [agentActivityOpen, setAgentActivityOpen] = useState(false);
  const [runContext, setRunContext] = useState<RunContext>();
  const [runContextBusy, setRunContextBusy] = useState(false);
  const [runContextError, setRunContextError] = useState(false);
  const [runContextReload, setRunContextReload] = useState(0);
  const [permissionMode, setPermissionMode] = useState<PermissionMode>("manual");
  const [reasoningEffort, setReasoningEffort] = useState<ReasoningEffort>();
  const [selectedModelName, setSelectedModelName] = useState<string>();
  const [continuityState, dispatchContinuity] = useReducer(
    reduceConversationContinuity,
    session.id,
    createConversationContinuityState,
  );
  const [liveEventState, dispatchLiveEvent] = useReducer(
    liveEventReducer,
    session.id,
    createLiveEventState,
  );
  const [streamStatus, setStreamStatus] = useState<RunStreamStatus>();
  const [submitting, setSubmitting] = useState(false);
  const [pendingPrompt, setPendingPrompt] = useState<PendingPrompt>();
  const [controlBusy, setControlBusy] = useState(false);
  const [verification, setVerification] = useState<VerificationSummary>();
  const [verificationBusy, setVerificationBusy] = useState(false);
  const [displayBusy, setDisplayBusy] = useState(false);
  const [displayError, setDisplayError] = useState(false);
  const [displayReload, setDisplayReload] = useState(0);
  const [attachmentGap, setAttachmentGap] = useState(false);
  const [continuityMessage, setContinuityMessage] = useState<string>();
  const [continuityRecoveryActions, setContinuityRecoveryActions] = useState<ContinuityRecoveryAction[]>([]);
  const continuityRecoveryActionsRef = useRef<readonly ContinuityRecoveryAction[]>([]);
  const [continuityReload, setContinuityReload] = useState(0);
  const [runAnnouncement, setRunAnnouncement] = useState("");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [extensionWorkbenchOpen, setExtensionWorkbenchOpen] = useState(false);
  const [extensionWorkbenchKind, setExtensionWorkbenchKind] = useState<"skills" | "agents">("skills");
  const [extensionWorkbenchQuery, setExtensionWorkbenchQuery] = useState("");
  const [requestedSkill, setRequestedSkill] = useState<SkillCatalogEntry>();
  const [requestedAgent, setRequestedAgent] = useState<AgentCatalogEntry>();
  const timelineRef = useRef<HTMLDivElement>(null);
  const timelinePinnedToEnd = useRef(true);
  const pendingTimelineAnchor = useRef<TimelineAnchor | undefined>(undefined);
  const activeRunIdRef = useRef<string | undefined>(undefined);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const inspectorTriggerRef = useRef<HTMLButtonElement>(null);
  const agentActivityTriggerRef = useRef<HTMLButtonElement>(null);
  const extensionTriggerRef = useRef<HTMLButtonElement>(null);
  const canonicalRefreshAttempts = useRef(0);
  const canonicalRefreshRunId = useRef<string | undefined>(undefined);
  const canonicalSettlementCandidate = useRef<string | undefined>(undefined);
  const pendingPromptCounter = useRef(0);
  const initialRunContextSessionId = useRef<string | undefined>(undefined);
  const initialDisplaySessionId = useRef<string | undefined>(undefined);
  const initialContinuitySessionId = useRef<string | undefined>(undefined);
  const initialLoadReportedSessionId = useRef<string | undefined>(undefined);
  const startRunPendingRef = useRef(false);
  const finishInitialLoadIfReady = useCallback((sessionId: string) => {
    if (
      initialLoadReportedSessionId.current !== sessionId
      && initialRunContextSessionId.current === sessionId
      && initialDisplaySessionId.current === sessionId
      && initialContinuitySessionId.current === sessionId
    ) {
      initialLoadReportedSessionId.current = sessionId;
      onInitialLoadComplete?.(sessionId);
    }
  }, [onInitialLoadComplete]);
  const onNotice = useCallback((message: string, error = false) => {
    if (!error) return;
    notify({ message, tone: "error" });
  }, [notify]);
  useEffect(() => {
    continuityRecoveryActionsRef.current = continuityRecoveryActions;
  }, [continuityRecoveryActions]);
  useEffect(() => {
    setRun(undefined);
    setAgentActivity(undefined);
    setAgentActivityBusy(false);
    setAgentActivityError(false);
    setAgentActivityOpen(false);
    setRunContext(undefined);
    setRunContextBusy(false);
    setRunContextError(false);
    setPermissionMode("manual");
    setReasoningEffort(undefined);
    setSelectedModelName(undefined);
    dispatchContinuity({ type: "session_selected", sessionId: session.id });
    dispatchLiveEvent({ type: "session_selected", sessionId: session.id });
    setPendingPrompt(undefined);
    setStreamStatus(undefined);
    setVerification(undefined);
    setDisplayError(false);
    setAttachmentGap(false);
    setContinuityMessage(undefined);
    setContinuityRecoveryActions([]);
    continuityRecoveryActionsRef.current = [];
    setRunAnnouncement("");
    setInspectorOpen(false);
    setExtensionWorkbenchOpen(false);
    setExtensionWorkbenchKind("skills");
    setExtensionWorkbenchQuery("");
    setRequestedSkill(undefined);
    setRequestedAgent(undefined);
    activeRunIdRef.current = undefined;
    canonicalRefreshAttempts.current = 0;
    canonicalRefreshRunId.current = undefined;
    canonicalSettlementCandidate.current = undefined;
    pendingPromptCounter.current = 0;
    initialRunContextSessionId.current = undefined;
    initialDisplaySessionId.current = undefined;
    initialContinuitySessionId.current = undefined;
    initialLoadReportedSessionId.current = undefined;
  }, [session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    setRunContextBusy(true);
    setRunContextError(false);
    void bridge
      .runContext(workspaceId, session.id)
      .then((context) => {
        if (disposed) return;
        setRunContext(context);
        onRunContextChange?.(context);
        setRunContextError(false);
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
        if (!disposed) {
          setRunContext(undefined);
          setRunContextError(true);
        }
      })
      .finally(() => {
        if (!disposed) {
          setRunContextBusy(false);
          initialRunContextSessionId.current = session.id;
          finishInitialLoadIfReady(session.id);
        }
      });
    return () => {
      disposed = true;
    };
  }, [bridge, finishInitialLoadIfReady, onRunContextChange, runContextReload, session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    setAgentActivityBusy(true);
    void bridge.agentActivity(workspaceId, session.id)
      .then((view) => {
        if (disposed) return;
        setAgentActivity(view);
        setAgentActivityError(false);
      })
      .catch(() => {
        if (!disposed) setAgentActivityError(true);
      })
      .finally(() => {
        if (!disposed) setAgentActivityBusy(false);
      });
    return () => {
      disposed = true;
    };
  }, [agentActivityReload, bridge, session.id, workspaceId]);

  useEffect(() => {
    let disposed = false;
    setDisplayBusy(true);
    void bridge
      .display(workspaceId, session.id, { limit: 50 })
      .then((page) => {
        if (disposed) return;
        const canonicalPage = toContinuityPage(page);
        dispatchContinuity({
          type: "initial_page_received",
          sessionId: session.id,
          page: canonicalPage,
        });
        dispatchLiveEvent({
          type: "anchor_received",
          sessionId: session.id,
          anchor: canonicalPage.liveProvisionalAnchor,
        });
        setDisplayError(false);
      })
      .catch(() => {
        if (!disposed) {
          setDisplayError(true);
          dispatchContinuity({
            type: "initial_page_failed",
            sessionId: session.id,
            message: "Canonical conversation display is unavailable.",
          });
          initialContinuitySessionId.current = session.id;
        }
      })
      .finally(() => {
        if (!disposed) {
          setDisplayBusy(false);
          if (initialDisplaySessionId.current !== session.id) {
            initialDisplaySessionId.current = session.id;
            finishInitialLoadIfReady(session.id);
          }
        }
      });
    return () => {
      disposed = true;
    };
  }, [bridge, displayReload, finishInitialLoadIfReady, session.id, workspaceId]);

  useEffect(() => {
    if (
      continuityState.lifecycle !== "error"
      || continuityState.transcriptLoaded
      || initialDisplaySessionId.current !== session.id
    ) return;
    initialContinuitySessionId.current = session.id;
    finishInitialLoadIfReady(session.id);
  }, [continuityState.lifecycle, continuityState.transcriptLoaded, finishInitialLoadIfReady, session.id]);

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
    if (!continuityState.transcriptLoaded || continuityState.contractError !== undefined) return;
    let disposed = false;
    let liveConnectionFailed = false;
    let continuityReprobeScheduled = false;
    let allowedRecoveryActions: ContinuityRecoveryAction[] = [];
    const unsubscribers: Array<() => void> = [];
    dispatchContinuity({ type: "owner_probe_started", sessionId: session.id });
    setContinuityMessage(undefined);
    setContinuityRecoveryActions([]);
    const settleInitialContinuity = () => {
      if (initialContinuitySessionId.current === session.id) return;
      initialContinuitySessionId.current = session.id;
      finishInitialLoadIfReady(session.id);
    };
    const enterRecovery = (
      message = t("liveControlsUnavailable"),
      error?: unknown,
    ) => {
      if (disposed) return;
      const projectedActions = continuityRecoveryActionsFromError(error);
      const actions = projectedActions ?? [...allowedRecoveryActions];
      dispatchContinuity({
        type: "owner_probe_failed",
        sessionId: session.id,
        message,
        canContinueReadOnly: actions.includes("continue_read_only"),
      });
      setContinuityMessage(message);
      setContinuityRecoveryActions(actions);
      settleInitialContinuity();
    };
    const scheduleContinuityReprobe = (runId?: string) => {
      if (disposed || continuityReprobeScheduled) return;
      continuityReprobeScheduled = true;
      activeRunIdRef.current = runId;
      dispatchContinuity({ type: "owner_probe_started", sessionId: session.id });
      setContinuityMessage(undefined);
      setContinuityReload((value) => value + 1);
    };
    const ingestEvent = (event: TimelineEvent) => {
      if (
        disposed
        || event.workspaceId !== workspaceId
        || event.sessionId !== session.id
      ) return;
      const previousRunId = activeRunIdRef.current;
      if (previousRunId === undefined) activeRunIdRef.current = event.runId;
      if (activeRunIdRef.current !== event.runId) return;

      dispatchLiveEvent({ type: "event_received", sessionId: session.id, event });
      const liveItem = semanticLiveItemFromTimelineEvent(event);
      if (liveItem !== undefined) {
        dispatchContinuity({ type: "live_item_received", sessionId: session.id, item: liveItem });
      }
      if (event.kind === "run_started") {
        setPendingPrompt((current) => (
          current !== undefined && (current.runId === undefined || current.runId === event.runId)
            ? undefined
            : current
        ));
      }
      if (event.kind === "control" && event.itemId?.startsWith("agent_")) {
        setAgentActivityReload((value) => value + 1);
      }

      const terminal = terminalSignalFromTimelineEvent(event);
      if (terminal !== undefined) {
        setPendingPrompt(undefined);
        dispatchContinuity({
          type: "terminal_observed",
          sessionId: session.id,
          terminal: { runId: terminal.runId, status: terminal.status },
        });
        const terminalStatus = terminalStatusForEvent(event);
        if (terminalStatus !== undefined) {
          setRun((current) =>
            current?.id === event.runId ? { ...current, status: terminalStatus } : current,
          );
        }
        scheduleContinuityReprobe();
      } else if (previousRunId === undefined && !startRunPendingRef.current) {
        scheduleContinuityReprobe(event.runId);
      }
    };
    const setup = async () => {
      const unsubscribeEvents = await bridge.subscribeRunEvents(ingestEvent);
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
        const previousRunId = activeRunIdRef.current;
        if (previousRunId === undefined) activeRunIdRef.current = status.runId;
        if (activeRunIdRef.current !== status.runId) return;
        setStreamStatus(status);
        if (status.state === "error") {
          liveConnectionFailed = true;
          enterRecovery(status.message ?? t("liveControlsUnavailable"));
        }
        if (status.state === "terminal") {
          dispatchContinuity({
            type: "terminal_transport_observed",
            sessionId: session.id,
            runId: status.runId,
          });
          setRunAnnouncement(status.message ?? t("runFinishedAnnouncement"));
          setRunContextReload((value) => value + 1);
          setAgentActivityReload((value) => value + 1);
          void bridge.verification(workspaceId, session.id).then(setVerification).catch(() => {
            setVerification(undefined);
          });
          scheduleContinuityReprobe();
        } else if (
          status.state !== "error"
          && previousRunId === undefined
          && !startRunPendingRef.current
        ) {
          scheduleContinuityReprobe(status.runId);
        }
      });
      if (disposed) {
        unsubscribeStatus();
        return;
      }
      unsubscribers.push(unsubscribeStatus);

      for (let attempt = 0; attempt < 3; attempt += 1) {
        const continuity = await bridge.continuity(workspaceId, session.id);
        if (disposed) return;
        allowedRecoveryActions = [...continuity.recoveryActions];
        setContinuityRecoveryActions(allowedRecoveryActions);
        const owner = continuity.foregroundOwner;
        if (owner === undefined) {
          activeRunIdRef.current = undefined;
          setRun(undefined);
          setStreamStatus(undefined);
          setAttachmentGap(false);
          dispatchContinuity({
            type: "owner_probe_resolved",
            sessionId: session.id,
          });
          setContinuityMessage(undefined);
          settleInitialContinuity();
          return;
        }

        activeRunIdRef.current = owner.runId;
        dispatchContinuity({
          type: "owner_probe_resolved",
          sessionId: session.id,
          foregroundOwner: { runId: owner.runId, ownerRevision: owner.ownerRevision },
        });
        try {
          const attachment = await bridge.attachRun(workspaceId, {
            sessionId: session.id,
            runId: owner.runId,
            ownerRevision: owner.ownerRevision,
          });
          if (disposed) return;
          setRun(attachment.run);
          setPermissionMode(attachment.run.permissionMode);
          setReasoningEffort(attachment.run.reasoningEffort);
          for (const event of attachment.events) ingestEvent(event);
          setStreamStatus({
            workspaceId,
            sessionId: session.id,
            runId: attachment.run.id,
            state: attachment.streamState,
            message: attachment.streamMessage,
          });
          setAttachmentGap(attachment.hasGap);
          if (isTerminal(attachment.run.status) || attachment.streamState === "terminal") {
            const terminal = terminalObservationFromRun(attachment.run);
            if (terminal !== undefined) {
              dispatchContinuity({
                type: "terminal_observed",
                sessionId: session.id,
                terminal,
              });
            } else {
              dispatchContinuity({
                type: "terminal_transport_observed",
                sessionId: session.id,
                runId: attachment.run.id,
              });
            }
            activeRunIdRef.current = undefined;
            continue;
          } else if (liveConnectionFailed || attachment.streamState === "error") {
            enterRecovery(attachment.streamMessage ?? t("liveControlsUnavailable"));
          } else {
            const confirmation = await bridge.continuity(workspaceId, session.id);
            if (disposed) return;
            allowedRecoveryActions = [...confirmation.recoveryActions];
            setContinuityRecoveryActions(allowedRecoveryActions);
            if (
              confirmation.foregroundOwner?.runId !== owner.runId
              || confirmation.foregroundOwner.ownerRevision !== owner.ownerRevision
            ) {
              continue;
            }
            dispatchContinuity({
              type: "run_attached",
              sessionId: session.id,
              runId: owner.runId,
              ownerRevision: owner.ownerRevision,
            });
            setContinuityMessage(undefined);
          }
          settleInitialContinuity();
          return;
        } catch (error) {
          if (attempt < 2 && isOwnerRace(error)) continue;
          throw error;
        }
      }
      throw new Error("continuity owner changed repeatedly");
    };
    void setup().catch((error: unknown) => enterRecovery(errorMessage(error) ?? t("liveControlsUnavailable"), error));
    return () => {
      disposed = true;
      for (const unsubscribe of unsubscribers) unsubscribe();
    };
  }, [bridge, continuityReload, continuityState.contractError, continuityState.transcriptLoaded, finishInitialLoadIfReady, session.id, t, workspaceId]);

  useEffect(() => {
    const observed = continuityState.observedTerminal;
    const pendingRunId = observed?.runId ?? continuityState.pendingTerminalRunId;
    if (
      pendingRunId === undefined
      || continuityState.refreshState !== "needed"
      || canonicalRefreshRunId.current === pendingRunId
    ) return;

    let cancelled = false;
    canonicalRefreshRunId.current = pendingRunId;
    pendingTimelineAnchor.current = timelinePinnedToEnd.current
      ? undefined
      : captureTimelineAnchor(timelineRef.current);
    dispatchContinuity({ type: "refresh_started", sessionId: session.id });
    const refresh = async () => {
      for (let attempt = 0; attempt < 4; attempt += 1) {
        canonicalRefreshAttempts.current = attempt + 1;
        if (attempt > 0) await waitForCanonicalProjection(attempt * 75);
        if (cancelled) return;
        try {
          const page = toContinuityPage(await bridge.display(workspaceId, session.id, { limit: 50 }));
          if (cancelled) return;
          dispatchContinuity({ type: "refresh_page_received", sessionId: session.id, page });
          dispatchLiveEvent({
            type: "anchor_received",
            sessionId: session.id,
            anchor: page.liveProvisionalAnchor,
          });
          if (canonicalPageCoversTerminal(page, {
            runId: pendingRunId,
            status: observed?.status,
          })) {
            canonicalRefreshAttempts.current = 0;
            canonicalRefreshRunId.current = undefined;
            canonicalSettlementCandidate.current = pendingRunId;
            return;
          }
        } catch {
          // A bounded retry absorbs the short window between terminal transport and durable projection.
        }
      }
      if (cancelled) return;
      canonicalRefreshRunId.current = undefined;
      dispatchContinuity({
        type: "refresh_failed",
        sessionId: session.id,
        message: t("savedMessagesRetryDetail"),
        canContinueReadOnly: continuityRecoveryActionsRef.current.includes("continue_read_only"),
      });
    };
    void refresh();
    return () => {
      cancelled = true;
      if (canonicalRefreshRunId.current === pendingRunId) {
        canonicalRefreshRunId.current = undefined;
      }
    };
  }, [bridge, continuityReload, continuityState.observedTerminal, continuityState.pendingTerminalRunId, session.id, t, workspaceId]);

  useEffect(() => {
    const candidate = canonicalSettlementCandidate.current;
    if (candidate === undefined) return;
    if (continuityState.contractError !== undefined) {
      canonicalSettlementCandidate.current = undefined;
      return;
    }
    if (
      continuityState.observedTerminal === undefined
      && continuityState.pendingTerminalRunId === undefined
      && continuityState.refreshState === "idle"
    ) {
      canonicalSettlementCandidate.current = undefined;
      dispatchLiveEvent({ type: "run_discarded", sessionId: session.id, runId: candidate });
    }
  }, [continuityState.contractError, continuityState.observedTerminal, continuityState.pendingTerminalRunId, continuityState.refreshState, session.id]);

  const rows = useMemo(() => {
    const next = projectConversationRows(
      selectConversationTimeline(continuityState),
      selectDeltaBuffers(liveEventState),
      t,
    );
    if (pendingPrompt === undefined) return next;
    return [...next, {
      key: pendingPrompt.identity,
      kind: "user" as const,
      label: t("you"),
      text: pendingPrompt.text,
      status: "sending",
    }];
  }, [continuityState, liveEventState, pendingPrompt, t]);
  const pendingApproval = useMemo(
    () => selectLatestPendingApproval(liveEventState),
    [liveEventState],
  );
  const active = run !== undefined && !isTerminal(run.status) && streamStatus?.state !== "terminal";
  const submissionBlocked = continuityState.contractError !== undefined
    || (continuityState.lifecycle !== "idle" && continuityState.lifecycle !== "live");

  useEffect(() => {
    if (pendingApproval?.approval !== undefined) setInspectorOpen(false);
  }, [pendingApproval?.approval]);

  useLayoutEffect(() => {
    const timeline = timelineRef.current;
    if (timeline === null) return;
    const anchor = pendingTimelineAnchor.current;
    if (anchor !== undefined) {
      // `refresh_started` deliberately changes reducer state before the
      // canonical page arrives. Keep the pre-refresh anchor alive through that
      // render so a live provisional identity can resolve to its durable
      // successor without moving the reader's viewport.
      if (continuityState.refreshState === "loading") return;
      const resolvedDisplayId = resolveConversationIdentity(continuityState, anchor.displayId);
      const anchorElement = [...timeline.querySelectorAll<HTMLElement>("[data-display-id]")]
        .find((element) => element.dataset.displayId === resolvedDisplayId);
      if (anchorElement !== undefined) {
        const nextOffset = anchorElement.getBoundingClientRect().top
          - timeline.getBoundingClientRect().top;
        timeline.scrollTop += nextOffset - anchor.viewportOffset;
      }
      pendingTimelineAnchor.current = undefined;
    } else if (timelinePinnedToEnd.current) {
      timeline.scrollTop = timeline.scrollHeight;
    }
  }, [continuityState, rows]);

  const loadEarlier = async () => {
    if (continuityState.nextCursor === undefined || displayBusy) return;
    pendingTimelineAnchor.current = captureTimelineAnchor(timelineRef.current);
    setDisplayBusy(true);
    try {
      const page = toContinuityPage(await bridge.display(workspaceId, session.id, {
        cursor: continuityState.nextCursor,
        limit: 50,
      }));
      dispatchContinuity({ type: "older_page_received", sessionId: session.id, page });
      setDisplayError(false);
    } catch {
      pendingTimelineAnchor.current = undefined;
      setDisplayError(true);
    } finally {
      setDisplayBusy(false);
    }
  };

  const submit = async (
    nextPrompt: string,
    skillBinding?: SkillBinding,
    agentBinding?: AgentBinding,
  ): Promise<boolean> => {
    if (nextPrompt === "" || active || submissionBlocked || submitting) return false;
    timelinePinnedToEnd.current = true;
    pendingPromptCounter.current += 1;
    setPendingPrompt({
      identity: `optimistic:${session.id}:${pendingPromptCounter.current}`,
      text: nextPrompt,
    });
    setSubmitting(true);
    startRunPendingRef.current = true;
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
      setPendingPrompt((current) => current === undefined ? current : { ...current, runId: started.id });
      setRun(started);
      dispatchContinuity({ type: "owner_probe_started", sessionId: session.id });
      setContinuityMessage(undefined);
      setPermissionMode(started.permissionMode);
      setReasoningEffort(started.reasoningEffort ?? selectedReasoningEffort);
      if (modelChanged) setRunContextReload((current) => current + 1);
      setContinuityReload((current) => current + 1);
      return true;
    } catch {
      setPendingPrompt(undefined);
      dispatchContinuity({ type: "recovery_retry_started", sessionId: session.id });
      setContinuityReload((current) => current + 1);
      onNotice(t("runStartFailed"), true);
      return false;
    } finally {
      startRunPendingRef.current = false;
      setSubmitting(false);
    }
  };

  const cancel = async () => {
    if (run === undefined || !active || submissionBlocked || controlBusy) return;
    setControlBusy(true);
    try {
      setRun(await bridge.cancelRun(workspaceId, session.id, run.id));
    } catch {
      onNotice(t("cancellationFailed"), true);
    } finally {
      setControlBusy(false);
    }
  };

  const decideApproval = async (decision: ApprovalAction) => {
    if (pendingApproval?.approval === undefined || continuityState.lifecycle !== "live" || controlBusy) return;
    setControlBusy(true);
    try {
      await bridge.resolveApproval(
        workspaceId,
        session.id,
        pendingApproval.runId,
        pendingApproval.approval,
        decision,
      );
    } catch {
      onNotice(t("approvalDecisionFailed"), true);
    } finally {
      setControlBusy(false);
    }
  };

  const rerunVerification = async () => {
    if (verification?.action?.kind !== "rerun" || verificationBusy || active || submissionBlocked) return;
    setVerificationBusy(true);
    try {
      const next = await bridge.rerunVerification(
        workspaceId,
        session.id,
        verification.action.request,
      );
      setVerification(next);
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
          <ConversationActivity state={streamStatus?.state} t={t} />
          {agentActivity !== undefined && agentActivity.totalAgents > 0 ? (
            <Tooltip label={t("openAgentActivity")}>
              <IconButton
                className={`agent-activity-trigger${agentActivity.activeAgents > 0 ? " is-active" : ""}`}
                ref={agentActivityTriggerRef}
                type="button"
                aria-label={t("openAgentActivity")}
                aria-controls="agent-activity-inspector"
                aria-expanded={agentActivityOpen}
                icon={(
                  <>
                    <Icon name="agents" />
                    <span className="agent-activity-trigger-count" aria-hidden="true">
                      {agentActivity.activeAgents > 0 ? agentActivity.activeAgents : agentActivity.totalAgents}
                    </span>
                  </>
                )}
                onClick={() => setAgentActivityOpen(true)}
              />
            </Tooltip>
          ) : null}
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

      {agentActivity !== undefined && agentActivity.activeAgents > 0 ? (
        <Button
          className="agent-activity-strip sg-bounded-content"
          variant="quiet"
          type="button"
          onClick={() => setAgentActivityOpen(true)}
        >
          <span className="agent-activity-pulse" aria-hidden="true" />
          <strong>{t("agentActiveCount", { count: agentActivity.activeAgents })}</strong>
          <span>{agentActivity.items.find((item) => !isTerminalAgentStatus(item.status))?.objective}</span>
          <small>{t("openAgentActivity")}</small>
        </Button>
      ) : null}

      {runContextError ? (
        <ErrorCard
          title={t("runContextUnavailable")}
          message={t("runContextUnavailableDetail")}
          actionLabel={runContextBusy ? t("retrying") : t("retryRunContext")}
          actionDisabled={runContextBusy}
          onAction={() => setRunContextReload((value) => value + 1)}
        />
      ) : null}

      {["loading_transcript", "checking_owner", "attaching_run", "finalizing"].includes(continuityState.lifecycle) ? (
        <section className="continuity-state sg-bounded-content" role="status">
          <span className="continuity-state-pulse" aria-hidden="true" />
          <div>
            <strong>{continuityState.lifecycle === "loading_transcript"
              ? t("loadingConversationHistory")
              : continuityState.lifecycle === "attaching_run"
                ? t("reattachingLiveRun")
                : continuityState.lifecycle === "finalizing"
                  ? t("runFinishedAnnouncement")
                  : t("checkingConversationContinuity")}</strong>
            <p>{continuityState.lifecycle === "finalizing"
              ? t("loadingSavedMessages")
              : t("checkingConversationContinuityDetail")}</p>
          </div>
        </section>
      ) : null}

      {continuityState.lifecycle === "read_only_recovery" || continuityState.lifecycle === "read_only" ? (
        <section className={`continuity-recovery sg-bounded-content${continuityState.lifecycle === "read_only" ? " is-read-only" : ""}`} role="alert">
          <span className="continuity-recovery-icon" aria-hidden="true"><Icon name="warning" /></span>
          <div className="continuity-recovery-copy">
            <strong>{continuityState.lifecycle === "read_only" ? t("conversationReadOnly") : t("liveControlsNeedRecovery")}</strong>
            <p>{continuityMessage ?? continuityState.recovery?.message ?? t("liveControlsUnavailable")}</p>
          </div>
          <div className="continuity-recovery-actions">
            {continuityRecoveryActions.includes("retry_current") ? (
              <Button type="button" variant="quiet" onClick={() => {
                dispatchContinuity({ type: "recovery_retry_started", sessionId: session.id });
                if (continuityState.transcriptLoaded) {
                  setContinuityReload((value) => value + 1);
                } else {
                  setDisplayReload((value) => value + 1);
                }
              }}>
                {t("retryLiveConnection")}
              </Button>
            ) : null}
            {continuityRecoveryActions.includes("open_another_workspace") ? (
              <Button type="button" variant="quiet" onClick={onOpenWorkspacePicker}>
                {t("openAnotherWorkspace")}
              </Button>
            ) : null}
            {continuityRecoveryActions.includes("open_diagnostics") ? (
              <Button type="button" variant="quiet" onClick={onOpenSupport}>
                {t("openSupport")}
              </Button>
            ) : null}
            {continuityState.lifecycle === "read_only_recovery"
              && continuityRecoveryActions.includes("continue_read_only")
              && !displayBusy
              && !displayError ? (
              <Button type="button" variant="quiet" onClick={() => dispatchContinuity({
                type: "continue_read_only",
                sessionId: session.id,
              })}>
                {t("continueReadOnly")}
              </Button>
            ) : null}
          </div>
          {continuityRecoveryActions.includes("show_details") ? (
            <details className="continuity-recovery-details">
              <summary>{t("showDetails")}</summary>
              <dl>
                <dt>{t("errorCode")}</dt>
                <dd>{continuityState.recovery?.code ?? "continuity_unavailable"}</dd>
                <dt>{t("errorMessage")}</dt>
                <dd>{continuityMessage ?? continuityState.recovery?.message ?? t("liveControlsUnavailable")}</dd>
              </dl>
            </details>
          ) : null}
        </section>
      ) : null}

      <div
        className="timeline sg-bounded-content"
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
        {attachmentGap || continuityState.gapFacts.length > 0 ? (
          <div className="timeline-gap" role="status">
            {t("liveDetailGap")}
          </div>
        ) : null}
        {continuityState.nextCursor !== undefined ? (
          <div className="transcript-pagination">
            <Button
              variant="quiet"
              type="button"
              disabled={displayBusy}
              onClick={() => void loadEarlier()}
            >
              {displayBusy
                ? t("loadingEarlierMessages")
                : t("loadEarlierMessages", { count: remainingDisplayItems(continuityState) })}
            </Button>
          </div>
        ) : null}
        {displayError || continuityState.contractError !== undefined ? (
          <ErrorCard
            title={t("savedMessagesUnavailable")}
            message={continuityState.contractError?.message ?? t("savedMessagesRetryDetail")}
            actionLabel={displayBusy ? t("retrying") : t("retryMessages")}
            actionDisabled={displayBusy}
            onAction={() => {
              dispatchContinuity({ type: "recovery_retry_started", sessionId: session.id });
              setDisplayReload((value) => value + 1);
            }}
          />
        ) : null}
        {rows.length === 0 ? (
          <div className="timeline-empty">
            <strong>{displayBusy ? t("loadingConversationHistory") : t("readyForPrompt")}</strong>
            <span>{displayBusy ? t("loadingSavedMessages") : t("newRunActivity")}</span>
          </div>
        ) : (
          rows.map((row) => row.kind === "tool"
            ? <ToolCard key={row.key} displayId={row.key} tool={{ key: row.key, toolName: row.label, text: row.text, input: row.input, status: row.status }} />
            : <Message key={row.key} displayId={row.key} message={row} onOpenExternalUrl={bridge.openExternalUrl} />)
        )}
      </div>

      {pendingApproval?.approval !== undefined && continuityState.lifecycle === "live" ? (
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
        submissionBlocked={submissionBlocked}
        draftEditingBlocked={!continuityState.transcriptLoaded}
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
        onOpenSettings={onOpenSettings}
        onOpenSupport={onOpenSupport}
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
          <VerificationInspector verification={verification} busy={verificationBusy} runActive={active || submissionBlocked} onRerun={() => void rerunVerification()} />
        </Drawer>
      ) : null}
      <Drawer
        id="agent-activity-inspector"
        open={agentActivityOpen}
        title={t("agentActivity")}
        description={t("agentActivityDetail")}
        returnFocusRef={agentActivityTriggerRef}
        onOpenChange={setAgentActivityOpen}
      >
        <AgentActivityPanel
          items={agentActivity?.items ?? []}
          error={agentActivityError}
          t={t}
        />
        {agentActivityBusy ? <span className="agent-activity-refreshing">{t("loading")}</span> : null}
      </Drawer>
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

function isTerminalAgentStatus(status: import("./types").AgentActivityStatus): boolean {
  return ["completed", "failed", "cancelled", "interrupted", "unavailable"].includes(status);
}

function ConversationActivity({
  state,
  t,
}: {
  readonly state?: RunStreamState;
  readonly t: Translate;
}) {
  const effectiveState = state ?? "idle";
  const label = (() => {
    switch (effectiveState) {
      case "idle": return t("conversationIdle");
      case "connecting": return t("conversationConnecting");
      case "live": return t("conversationLive");
      case "reconnecting": return t("conversationReconnecting");
      case "terminal": return t("conversationTerminal");
      case "error": return t("conversationStreamError");
    }
  })();
  return (
    <span className={`conversation-activity stream-${effectiveState}`} role="status">
      <span className="conversation-activity-dot" aria-hidden="true" />
      {label}
    </span>
  );
}

const TERMINAL_DISPLAY_STATUSES: readonly ConversationTerminalObservation["status"][] = [
  "succeeded",
  "failed",
  "cancelled",
  "interrupted",
] as const;

function toContinuityPage(page: BridgeConversationDisplayPage): ConversationDisplayPage {
  if (page.items.some((item) => item.source === "live_transient")) {
    throw new Error("The canonical conversation projection returned a transient item.");
  }
  if (
    page.terminalFrontier !== undefined
    && !TERMINAL_DISPLAY_STATUSES.includes(
      page.terminalFrontier.status as ConversationTerminalObservation["status"],
    )
  ) {
    throw new Error("The canonical conversation projection returned a non-terminal frontier.");
  }
  return page as ConversationDisplayPage;
}

function terminalObservationFromRun(
  run: RunSummary,
): ConversationTerminalObservation | undefined {
  switch (run.status) {
    case "finished":
      return { runId: run.id, status: "succeeded" };
    case "failed":
      return { runId: run.id, status: "failed" };
    case "cancelled":
      return { runId: run.id, status: "cancelled" };
    case "interrupted":
      return { runId: run.id, status: "interrupted" };
    default:
      return undefined;
  }
}

function canonicalPageCoversTerminal(
  page: ConversationDisplayPage,
  observed: { runId: string; status?: ConversationTerminalObservation["status"] },
): boolean {
  const terminal = page.terminalFrontier;
  return terminal !== undefined
    && terminal.runId === observed.runId
    && (observed.status === undefined || terminal.status === observed.status)
    && BigInt(page.throughSessionStreamSequence) >= BigInt(terminal.sessionStreamSequence);
}

function captureTimelineAnchor(timeline: HTMLDivElement | null): TimelineAnchor | undefined {
  if (timeline === null) return undefined;
  const timelineTop = timeline.getBoundingClientRect().top;
  const element = [...timeline.querySelectorAll<HTMLElement>("[data-display-id]")]
    .find((candidate) => candidate.getBoundingClientRect().bottom >= timelineTop);
  return element?.dataset.displayId === undefined
    ? undefined
    : {
      displayId: element.dataset.displayId,
      viewportOffset: element.getBoundingClientRect().top - timelineTop,
    };
}

function remainingDisplayItems(state: ConversationContinuityState): string {
  if (state.totalItems === undefined) return "0";
  const remaining = BigInt(state.totalItems) - BigInt(state.canonicalItems.size);
  const zero = BigInt(0);
  return (remaining > zero ? remaining : zero).toString();
}

function waitForCanonicalProjection(delayMs: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, delayMs));
}

function isOwnerRace(error: unknown): boolean {
  if (typeof error !== "object" || error === null || !("code" in error)) return false;
  const code = (error as { code?: unknown }).code;
  return code === "run_no_longer_foreground" || code === "run_owner_changed";
}

const CONTINUITY_RECOVERY_ACTIONS: readonly ContinuityRecoveryAction[] = [
  "retry_current",
  "open_another_workspace",
  "open_diagnostics",
  "show_details",
  "continue_read_only",
];

function continuityRecoveryActionsFromError(error: unknown): ContinuityRecoveryAction[] | undefined {
  if (typeof error !== "object" || error === null || !("recoveryActions" in error)) return undefined;
  const actions = error.recoveryActions;
  if (!Array.isArray(actions)) return undefined;
  const allowed = new Set<string>(CONTINUITY_RECOVERY_ACTIONS);
  return [...new Set(actions.filter((action): action is ContinuityRecoveryAction =>
    typeof action === "string" && allowed.has(action),
  ))];
}

function errorMessage(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("message" in error)) return undefined;
  return typeof error.message === "string" ? error.message : undefined;
}

function isTerminal(status: RunSummary["status"]): boolean {
  return ["finished", "failed", "cancelled", "interrupted"].includes(status);
}

function terminalStatusForEvent(event: TimelineEvent): RunSummary["status"] | undefined {
  if (event.kind === "run_finished") return "finished";
  if (event.kind === "run_failed") {
    return event.status === "interrupted" ? "interrupted" : "failed";
  }
  if (event.kind === "run_cancelled") return "cancelled";
  return undefined;
}
