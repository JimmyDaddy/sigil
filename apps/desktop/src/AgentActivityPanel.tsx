import { MessageContent } from "./MessageContent";
import type { AgentActivityItem, AgentActivityStatus, AgentHandoffStatus } from "./types";
import type { Translate } from "./i18n";

export function AgentActivityPanel({
  items,
  error,
  t,
}: {
  readonly items: AgentActivityItem[];
  readonly error: boolean;
  readonly t: Translate;
}) {
  if (error) return <p className="agent-activity-empty">{t("agentActivityUnavailable")}</p>;
  if (items.length === 0) return <p className="agent-activity-empty">{t("noAgentActivity")}</p>;
  return (
    <div className="agent-activity-list sg-bounded-content">
      {items.map((item) => (
        <article className={`agent-activity-card status-${item.status}`} key={item.threadId}>
          <header>
            <span>
              <strong>{item.displayName ?? item.profileId ?? item.threadId}</strong>
              {item.profileId !== undefined && item.displayName !== undefined
                ? <small>@{item.profileId}</small>
                : null}
            </span>
            <span className="agent-activity-badges">
              <small>{agentStatusLabel(item.status, t)}</small>
              <small>{agentHandoffLabel(item.handoffStatus, t)}</small>
            </span>
          </header>
          <section>
            <span>{t("agentObjective")}</span>
            <p>{item.objective}</p>
          </section>
          {item.resultSummary !== undefined ? (
            <section>
              <span>{t("agentResult")}</span>
              <MessageContent text={item.resultSummary} />
              {item.resultSummaryTruncated ? <small>{t("agentResultTruncated")}</small> : null}
            </section>
          ) : item.reason !== undefined ? (
            <section>
              <span>{t("agentResult")}</span>
              <p>{item.reason}</p>
            </section>
          ) : null}
          {item.usage !== undefined ? (
            <footer>
              <span>{t("agentUsage")}</span>
              <small>{t("agentTokens", { count: item.usage.totalTokens })}</small>
            </footer>
          ) : null}
        </article>
      ))}
    </div>
  );
}

function agentStatusLabel(status: AgentActivityStatus, t: Translate): string {
  switch (status) {
    case "started": return t("agentStatusStarted");
    case "running": return t("agentStatusRunning");
    case "blocked": return t("agentStatusBlocked");
    case "completed": return t("agentStatusCompleted");
    case "failed": return t("agentStatusFailed");
    case "cancelled": return t("agentStatusCancelled");
    case "interrupted": return t("agentStatusInterrupted");
    case "unavailable": return t("agentStatusUnavailable");
    case "unknown": return t("agentStatusUnknown");
  }
}

function agentHandoffLabel(status: AgentHandoffStatus, t: Translate): string {
  switch (status) {
    case "pending": return t("agentHandoffPending");
    case "result_ready": return t("agentHandoffResultReady");
    case "result_read": return t("agentHandoffResultRead");
    case "returned": return t("agentHandoffReturned");
    case "unavailable": return t("agentHandoffUnavailable");
  }
}
