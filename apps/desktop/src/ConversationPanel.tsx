import { useEffect, useMemo, useState } from "react";

import type { DesktopBridge } from "./bridge";
import type {
  RunStreamStatus,
  RunSummary,
  SessionSummary,
  TimelineEvent,
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

  useEffect(() => {
    setRun(undefined);
    setEvents([]);
    setStreamStatus(undefined);
  }, [session.id, workspaceId]);

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

  const rows = useMemo(() => reduceTimeline(events), [events]);
  const active = run !== undefined && !isTerminal(run.status) && streamStatus?.state !== "terminal";

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

      <div className="timeline" aria-live="polite" aria-label="Conversation timeline">
        {rows.length === 0 ? (
          <div className="timeline-empty">
            <strong>Ready for a prompt.</strong>
            <span>New run events appear here. Earlier message bodies remain in durable storage and are not copied into the desktop catalog.</span>
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
        />
        <div className="composer-actions">
          <small>{active ? "One foreground run is active." : "Approval mode: ask"}</small>
          <button className="primary-button" type="submit" disabled={prompt.trim() === "" || active || submitting}>
            {submitting ? "Starting…" : "Run"}
          </button>
        </div>
      </form>
    </section>
  );
}

export function mergeTimelineEvent(
  current: TimelineEvent[],
  incoming: TimelineEvent,
): TimelineEvent[] {
  const key = eventKey(incoming);
  if (current.some((event) => eventKey(event) === key)) return current;
  return [...current, incoming].sort((left, right) =>
    left.runId === right.runId
      ? left.sequence - right.sequence
      : left.runId.localeCompare(right.runId),
  );
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
