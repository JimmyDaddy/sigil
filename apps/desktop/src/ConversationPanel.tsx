import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import type { DesktopBridge } from "./bridge";
import type {
  RunStreamStatus,
  RunSummary,
  SessionSummary,
  TimelineEvent,
  TranscriptMessage,
  VerificationSummary,
} from "./types";

interface ConversationPanelProps {
  bridge: DesktopBridge;
  workspaceId: string;
  session: SessionSummary;
  onNotice(message: string, error?: boolean): void;
}

interface TimelineRow {
  key: string;
  kind: "user" | "assistant" | "reasoning" | "tool" | "notice" | "error";
  label: string;
  text: string;
  status?: string;
}

export function ConversationPanel({
  bridge,
  workspaceId,
  session,
  onNotice,
}: ConversationPanelProps) {
  const [prompt, setPrompt] = useState("");
  const [run, setRun] = useState<RunSummary>();
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
  const timelineRef = useRef<HTMLDivElement>(null);
  const timelinePinnedToEnd = useRef(true);
  const prependScrollHeight = useRef<number | undefined>(undefined);

  useEffect(() => {
    setRun(undefined);
    setEvents([]);
    setStreamStatus(undefined);
    setVerification(undefined);
    setTranscript([]);
    setTranscriptTotal(0);
    setNextBefore(undefined);
    setTranscriptError(false);
  }, [session.id, workspaceId]);

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
    void bridge.subscribeRunEvents((event) => {
      if (
        !disposed &&
        event.workspaceId === workspaceId &&
        event.sessionId === session.id
      ) {
        setEvents((current) => mergeTimelineEvent(current, event));
      }
    }).then((unsubscribe) => {
      if (disposed) unsubscribe();
      else unsubscribers.push(unsubscribe);
    });
    void bridge.subscribeRunStreamStatus((status) => {
      if (
        !disposed &&
        status.workspaceId === workspaceId &&
        status.sessionId === session.id
      ) {
        setStreamStatus(status);
        if (status.message !== undefined) onNotice(status.message, status.state === "error");
        if (status.state === "terminal") {
          void bridge.verification(workspaceId, session.id).then(setVerification).catch(() => {
            setVerification(undefined);
          });
        }
      }
    }).then((unsubscribe) => {
      if (disposed) unsubscribe();
      else unsubscribers.push(unsubscribe);
    });
    return () => {
      disposed = true;
      for (const unsubscribe of unsubscribers) unsubscribe();
    };
  }, [bridge, onNotice, session.id, workspaceId]);

  const rows = useMemo(
    () => [...reduceTranscript(transcript), ...reduceTimeline(events)],
    [events, transcript],
  );
  const pendingApproval = useMemo(() => latestPendingApproval(events), [events]);
  const active = run !== undefined && !isTerminal(run.status) && streamStatus?.state !== "terminal";

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

  const submit = async () => {
    const nextPrompt = prompt.trim();
    if (nextPrompt === "" || active || submitting) return;
    setSubmitting(true);
    onNotice("Starting the run…");
    try {
      const started = await bridge.startRun(workspaceId, session.id, nextPrompt);
      setRun(started);
      setPrompt("");
      onNotice("Run started. Live updates are connected.");
    } catch {
      onNotice("The run could not be started.", true);
    } finally {
      setSubmitting(false);
    }
  };

  const cancel = async () => {
    if (run === undefined || !active || controlBusy) return;
    setControlBusy(true);
    onNotice("Requesting cooperative cancellation…");
    try {
      setRun(await bridge.cancelRun(workspaceId, session.id, run.id));
      onNotice("Cancellation requested. Waiting for durable cleanup evidence.");
    } catch {
      onNotice("Cancellation could not be requested.", true);
    } finally {
      setControlBusy(false);
    }
  };

  const decideApproval = async (approve: boolean) => {
    if (pendingApproval?.approval === undefined || controlBusy) return;
    setControlBusy(true);
    onNotice(approve ? "Submitting approval…" : "Submitting denial…");
    try {
      const decision = await bridge.resolveApproval(
        workspaceId,
        session.id,
        pendingApproval.runId,
        pendingApproval.approval,
        approve,
      );
      onNotice(`Tool request ${decision.decision}.`);
    } catch {
      onNotice("The approval decision was stale or could not be recorded.", true);
    } finally {
      setControlBusy(false);
    }
  };

  const rerunVerification = async () => {
    if (verification?.action?.kind !== "rerun" || verificationBusy || active) return;
    setVerificationBusy(true);
    onNotice(`Running recommended check ${verification.recommendedCheckSpecId ?? ""}…`);
    try {
      const next = await bridge.rerunVerification(
        workspaceId,
        session.id,
        verification.action.request,
      );
      setVerification(next);
      onNotice(`Verification finished: ${next.status}.`);
    } catch {
      onNotice("The verification binding was stale or the check could not run.", true);
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
    <section className="conversation-panel" aria-labelledby="conversation-title">
      <header className="conversation-header">
        <div>
          <p className="eyebrow">Active conversation</p>
          <h2 id="conversation-title">{session.label ?? "Untitled conversation"}</h2>
        </div>
        <span className={`stream-chip stream-${streamStatus?.state ?? "idle"}`}>
          {streamStatus?.state ?? "ready"}
        </span>
      </header>

      <div
        className="timeline"
        ref={timelineRef}
        role="log"
        aria-live="polite"
        aria-relevant="additions text"
        aria-label="Conversation timeline"
        onScroll={(event) => {
          const timeline = event.currentTarget;
          timelinePinnedToEnd.current =
            timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight <= 48;
        }}
      >
        {nextBefore !== undefined ? (
          <div className="transcript-pagination">
            <button
              className="quiet-button"
              type="button"
              disabled={transcriptBusy}
              onClick={() => void loadEarlier()}
            >
              {transcriptBusy ? "Loading earlier messages…" : `Load earlier messages (${Math.max(0, transcriptTotal - transcript.length)} remaining)`}
            </button>
          </div>
        ) : null}
        {transcriptError ? (
          <div className="timeline-history-error" role="status">
            <span>Conversation history could not be loaded. Live run controls remain available.</span>
            <button
              className="quiet-button"
              type="button"
              disabled={transcriptBusy}
              onClick={() => setTranscriptReload((value) => value + 1)}
            >
              Retry history
            </button>
          </div>
        ) : null}
        {rows.length === 0 ? (
          <div className="timeline-empty">
            <strong>{transcriptBusy ? "Loading conversation history…" : "Ready for a prompt."}</strong>
            <span>{transcriptBusy ? "Reading a bounded page from durable session history." : "New run events appear here."}</span>
          </div>
        ) : (
          rows.map((row) => (
            <article className={`timeline-row timeline-${row.kind}`} key={row.key}>
              <header><span>{row.label}</span>{row.status ? <small>{row.status}</small> : null}</header>
              <p>{row.text || "No text payload."}</p>
            </article>
          ))
        )}
      </div>

      {pendingApproval?.approval !== undefined ? (
        <section className="approval-card" aria-labelledby="approval-title">
          <header>
            <div>
              <p className="eyebrow">Explicit approval required</p>
              <h3 id="approval-title">{pendingApproval.approval.previewTitle ?? pendingApproval.approval.toolName}</h3>
            </div>
            <span className={`risk-badge risk-${pendingApproval.approval.risk ?? "unknown"}`}>
              {pendingApproval.approval.risk ?? "unclassified"}
            </span>
          </header>
          <p>{pendingApproval.approval.previewSummary ?? "Review this tool request before it can continue."}</p>
          {pendingApproval.approval.previewBody ? <pre>{pendingApproval.approval.previewBody}</pre> : null}
          <dl>
            <div><dt>Tool</dt><dd>{pendingApproval.approval.toolName}</dd></div>
            <div><dt>Operation</dt><dd>{pendingApproval.approval.operation ?? "unknown"}</dd></div>
            <div><dt>Snapshot</dt><dd>{pendingApproval.approval.snapshotRequired ? "required" : "not required"}</dd></div>
          </dl>
          <small>Approval applies only to this exact request. Shell and remote side effects cannot be undone by desktop history controls.</small>
          <div className="approval-actions">
            <button className="quiet-button danger-button" type="button" disabled={controlBusy} onClick={() => void decideApproval(false)}>Deny</button>
            <button className="primary-button" type="button" disabled={controlBusy} onClick={() => void decideApproval(true)}>Approve once</button>
          </div>
        </section>
      ) : null}

      {verification !== undefined ? (
        <section className="verification-card" aria-labelledby="verification-title">
          <header>
            <div>
              <p className="eyebrow">Verification</p>
              <h3 id="verification-title">{verification.recommendedCheckSpecId ?? "Current evidence"}</h3>
            </div>
            <span className={`verification-badge verification-${verification.verdict}`}>
              {verification.status}
            </span>
          </header>
          {verification.recommendationReason ? <p>{verification.recommendationReason}</p> : null}
          <dl>
            <div><dt>Scope</dt><dd>{verification.scopeKind} · {verification.scopeId}</dd></div>
            <div><dt>Receipt</dt><dd>{verification.evidence.receiptId ?? "not recorded"}</dd></div>
            <div><dt>Snapshot</dt><dd>{verification.evidence.workspaceSnapshotId ?? "not linked"}</dd></div>
            <div><dt>Changeset</dt><dd>{verification.evidence.changesetId ?? "not linked"}</dd></div>
          </dl>
          {verification.evidence.failureSummary ? (
            <div className="verification-failure" role="status">
              <strong>Failure location</strong>
              <p>{verification.evidence.failureSummary}</p>
              <small>
                Command {verification.evidence.commandEventId ?? "not linked"} · output {verification.evidence.outputArtifactId ?? "not linked"}
              </small>
            </div>
          ) : null}
          <div className="verification-actions">
            {verification.action?.kind === "rerun" ? (
              <button
                className="primary-button"
                type="button"
                disabled={verificationBusy || active}
                onClick={() => void rerunVerification()}
              >
                {verificationBusy
                  ? "Running check…"
                  : verification.recommendationKind === "retry"
                    ? "Retry check"
                    : verification.recommendationKind === "rerun_non_writing"
                      ? "Rerun non-writing check"
                      : "Run recommended check"}
              </button>
            ) : verification.action?.kind === "review_approval" ? (
              <small>This check needs a separate trust review. Desktop does not silently promote repository commands.</small>
            ) : (
              <small>No verification action is currently required.</small>
            )}
          </div>
        </section>
      ) : null}

      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <label htmlFor="desktop-prompt">Message Sigil</label>
        <textarea
          id="desktop-prompt"
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          placeholder="Describe the change or question…"
          rows={4}
          disabled={active || submitting}
          onCompositionStart={(event) => {
            event.currentTarget.dataset.composing = "true";
          }}
          onCompositionEnd={(event) => {
            delete event.currentTarget.dataset.composing;
          }}
        />
        <div className="composer-actions">
          <small>{active ? "One foreground run is active." : "Approval mode: ask"}</small>
          <div>
            {active ? <button className="quiet-button danger-button" type="button" disabled={controlBusy} onClick={() => void cancel()}>Cancel run</button> : null}
            <button className="primary-button" type="submit" disabled={prompt.trim() === "" || active || submitting}>
              {submitting ? "Starting…" : "Run"}
            </button>
          </div>
        </div>
      </form>
    </section>
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
  return [...pending.values()].at(-1);
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

export function reduceTranscript(messages: TranscriptMessage[]): TimelineRow[] {
  return messages.map((message) => {
    const attachmentText = message.imageAttachmentCount > 0
      ? `${message.imageAttachmentCount} image attachment${message.imageAttachmentCount === 1 ? "" : "s"} recorded.`
      : "";
    const text = (message.content ?? attachmentText) || "No text payload.";
    const status = message.truncated
      ? `preview · ${message.originalContentBytes} bytes`
      : message.imageAttachmentCount > 0
        ? `${message.imageAttachmentCount} attachment${message.imageAttachmentCount === 1 ? "" : "s"}`
        : undefined;
    if (message.role === "user") {
      return {
        key: `history:${message.messageId}`,
        kind: "user",
        label: "You",
        text,
        status,
      };
    }
    if (message.role === "tool") {
      return {
        key: `history:${message.messageId}`,
        kind: "tool",
        label: message.toolName ?? "Tool result",
        text,
        status,
      };
    }
    if (message.assistantKind === "reasoning_trace") {
      return {
        key: `history:${message.messageId}`,
        kind: "reasoning",
        label: "Reasoning",
        text,
        status,
      };
    }
    if (message.assistantKind === "progress") {
      return {
        key: `history:${message.messageId}`,
        kind: "notice",
        label: "Progress",
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

export function reduceTimeline(events: TimelineEvent[]): TimelineRow[] {
  const rows = new Map<string, TimelineRow>();
  for (const event of events) {
    const assistantKey = `${event.runId}:assistant`;
    switch (event.kind) {
      case "run_started":
        rows.set(`${event.runId}:user`, {
          key: `${event.runId}:user`, kind: "user", label: "You", text: event.text ?? "",
        });
        break;
      case "assistant_delta": {
        const current = rows.get(assistantKey);
        rows.set(assistantKey, {
          key: assistantKey,
          kind: "assistant",
          label: "Sigil",
          text: `${current?.text ?? ""}${event.text ?? ""}`,
          status: "streaming",
        });
        break;
      }
      case "assistant_message":
        rows.set(assistantKey, {
          key: assistantKey,
          kind: "assistant",
          label: "Sigil",
          text: event.text ?? rows.get(assistantKey)?.text ?? "",
          status: "complete",
        });
        break;
      case "run_finished": {
        const current = rows.get(assistantKey);
        if (current === undefined || current.text === "") {
          rows.set(assistantKey, {
            key: assistantKey,
            kind: "assistant",
            label: "Sigil",
            text: event.text ?? "Run completed.",
            status: "complete",
          });
        } else {
          rows.set(assistantKey, { ...current, status: "complete" });
        }
        break;
      }
      case "reasoning_delta": {
        const key = `${event.runId}:reasoning`;
        const current = rows.get(key);
        rows.set(key, {
          key,
          kind: "reasoning",
          label: "Working",
          text: `${current?.text ?? ""}${event.text ?? ""}`,
        });
        break;
      }
      case "tool_started":
      case "tool_completed":
      case "tool_progress":
      case "tool_result": {
        const key = `${event.runId}:tool:${event.itemId ?? event.sequence}`;
        rows.set(key, {
          key,
          kind: "tool",
          label: event.toolName ?? "Tool",
          text: event.text ?? "Tool activity",
          status: event.status ?? event.kind.replace("tool_", ""),
        });
        break;
      }
      case "approval_requested": {
        const key = `${event.runId}:approval:${event.itemId ?? event.sequence}`;
        rows.set(key, {
          key,
          kind: "notice",
          label: "Approval required",
          text: `${event.toolName ?? "Tool"} is waiting for a decision.`,
          status: "waiting",
        });
        break;
      }
      case "run_failed":
      case "run_cancelled":
        rows.set(`${event.runId}:terminal`, {
          key: `${event.runId}:terminal`,
          kind: "error",
          label: event.kind === "run_cancelled" ? "Cancelled" : "Run failed",
          text: event.text ?? (event.kind === "run_cancelled" ? "The run was cancelled." : "The run failed."),
        });
        break;
      case "notice":
        rows.set(`${event.runId}:notice:${event.sequence}`, {
          key: `${event.runId}:notice:${event.sequence}`,
          kind: "notice",
          label: "Notice",
          text: event.text ?? "Run notice",
        });
        break;
      default:
        break;
    }
  }
  return [...rows.values()];
}

function eventKey(event: TimelineEvent): string {
  return `${event.runId}:${event.sequence}:${event.kind}`;
}

function isTerminal(status: RunSummary["status"]): boolean {
  return ["finished", "failed", "cancelled", "interrupted"].includes(status);
}
