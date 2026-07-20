import type { CatalogEntry, CatalogPage, CatalogSourceState } from "./types";
import { ErrorCard } from "./ErrorCard";
import { Icon } from "./ui/icons";
import { Button, Popover } from "./ui/primitives";

export type HistoryState =
  | "idle"
  | "loading"
  | "ready"
  | "loading_more"
  | "stale"
  | "error";

export function HistoryContent({
  state,
  page,
  onRetry,
  onLoadMore,
  onOpen,
  selectedSessionId,
}: {
  state: HistoryState;
  page: CatalogPage;
  onRetry: () => void;
  onLoadMore: () => void;
  onOpen: (entry: CatalogEntry) => void;
  selectedSessionId?: string;
}) {
  if (state === "loading") {
    return <div className="history-notice busy">Loading conversations…</div>;
  }
  if (state === "error" || state === "stale") {
    return (
      <ErrorCard
        title={state === "stale" ? "History changed while paging." : "History is unavailable."}
        message={state === "stale" ? "The list changed while more items were loading. Refresh and continue." : "Your saved conversations are unchanged. Try loading the list again."}
        actionLabel="Refresh conversations"
        onAction={onRetry}
      />
    );
  }
  if (state === "idle") return null;

  const hasWarnings = page.degradedSourceCount > 0 || page.identityConflictCount > 0 || page.truncatedSourceCount > 0;
  const warningCount = page.degradedSourceCount + page.identityConflictCount + page.truncatedSourceCount;
  const groups = groupEntries(page.entries, page.reconciledAtUnixMs);
  return (
    <div className="history-results">
      <div className="history-meta">
        <span>{page.entries.length} conversations</span>
        <span className="history-meta-actions">
          <small>Updated {formatTime(page.reconciledAtUnixMs)}</small>
          {hasWarnings ? (
            <Popover
              className="catalog-diagnostics"
              label={<span className="catalog-diagnostics-trigger"><Icon name="warning" /><span>{warningCount}</span></span>}
              accessibleLabel={`Catalog diagnostics, ${warningCount} issue${warningCount === 1 ? "" : "s"}`}
            >
              <div className="catalog-diagnostics-panel">
                <strong>Catalog diagnostics</strong>
                <p>Some saved conversation sources need attention.</p>
                <ul>
                  {page.degradedSourceCount > 0 ? <li><span>Unavailable</span><strong>{page.degradedSourceCount}</strong></li> : null}
                  {page.identityConflictCount > 0 ? <li><span>Changed</span><strong>{page.identityConflictCount}</strong></li> : null}
                  {page.truncatedSourceCount > 0 ? <li><span>Too large to inspect</span><strong>{page.truncatedSourceCount}</strong></li> : null}
                </ul>
              </div>
            </Popover>
          ) : null}
        </span>
      </div>
      {page.entries.length === 0 ? (
        <div className="history-empty">
          <strong>No matching conversation.</strong>
          <p>Start a new conversation or adjust the filters.</p>
        </div>
      ) : (
        <div className="history-groups">
          {groups.map((group) => (
            <section className="history-group" key={group.id} aria-labelledby={`history-group-${group.id}`}>
              <h3 id={`history-group-${group.id}`}>{group.label}</h3>
              <ul className="history-list">
                {group.entries.map((entry) => {
                  const canOpen = entry.sourceState === "ready" && entry.sessionId !== undefined;
                  const providerContext = [entry.providerName, entry.modelName].filter(Boolean).join(" · ");
                  const content = (
                    <>
                      <span className="session-row-title">
                        <strong>{entry.title ?? "Untitled conversation"}</strong>
                        {entry.pinned ? <span className="pin-indicator" aria-label="Pinned"><Icon name="pin" /></span> : null}
                        {entry.sourceState === "ready" ? null : <span className={`source-badge source-${entry.sourceState}`}>{sourceLabel(entry.sourceState)}</span>}
                      </span>
                      <small>
                        {formatActivityCount(entry)} · {formatRelativeTime(entry.sourceModifiedAtUnixMs, page.reconciledAtUnixMs)}
                      </small>
                    </>
                  );
                  return (
                    <li key={`${entry.sessionRef}:${entry.sessionId ?? entry.sourceState}`}>
                      {canOpen ? (
                        <Button
                          className="session-row"
                          type="button"
                          variant="quiet"
                          aria-current={entry.sessionId === selectedSessionId ? "page" : undefined}
                          title={providerContext || undefined}
                          onClick={() => onOpen(entry)}
                        >
                          {content}
                        </Button>
                      ) : (
                        <div className="session-row session-row-unavailable" aria-disabled="true" title={providerContext || undefined}>{content}</div>
                      )}
                    </li>
                  );
                })}
              </ul>
            </section>
          ))}
        </div>
      )}
      {page.nextCursor !== undefined ? (
        <Button className="load-more" type="button" onClick={onLoadMore} disabled={state === "loading_more"}>
          {state === "loading_more" ? "Loading…" : "Load more"}
        </Button>
      ) : null}
    </div>
  );
}

type HistoryGroup = {
  readonly id: "today" | "yesterday" | "earlier";
  readonly label: "Today" | "Yesterday" | "Earlier";
  readonly entries: CatalogEntry[];
};

export function groupEntries(entries: readonly CatalogEntry[], referenceTimestamp: number): HistoryGroup[] {
  const reference = new Date(referenceTimestamp > 0 ? referenceTimestamp : Date.now());
  const todayStart = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate()).getTime();
  const yesterdayStart = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate() - 1).getTime();
  const grouped: Record<HistoryGroup["id"], CatalogEntry[]> = { today: [], yesterday: [], earlier: [] };

  for (const entry of entries) {
    if (entry.sourceModifiedAtUnixMs >= todayStart) grouped.today.push(entry);
    else if (entry.sourceModifiedAtUnixMs >= yesterdayStart) grouped.yesterday.push(entry);
    else grouped.earlier.push(entry);
  }

  const labels: Record<HistoryGroup["id"], HistoryGroup["label"]> = {
    today: "Today",
    yesterday: "Yesterday",
    earlier: "Earlier",
  };
  return (["today", "yesterday", "earlier"] as const)
    .filter((id) => grouped[id].length > 0)
    .map((id) => ({ id, label: labels[id], entries: grouped[id] }));
}

function sourceLabel(state: CatalogSourceState): string {
  switch (state) {
    case "ready": return "Ready";
    case "oversized": return "Oversized";
    case "scan_budget_exceeded": return "Scan limited";
    case "unsupported_legacy": return "Unavailable";
    case "invalid": return "Invalid";
  }
}

function formatTime(value: number): string {
  if (value <= 0) return "just now";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function formatRelativeTime(value: number, referenceTimestamp: number): string {
  if (value <= 0) return "Just now";
  const reference = referenceTimestamp > 0 ? referenceTimestamp : Date.now();
  const elapsedMinutes = Math.max(0, Math.floor((reference - value) / 60_000));
  if (elapsedMinutes < 1) return "Just now";
  if (elapsedMinutes < 60) return `${elapsedMinutes}m`;
  const elapsedHours = Math.floor(elapsedMinutes / 60);
  if (elapsedHours < 24) return `${elapsedHours}h`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(new Date(value));
}

function formatActivityCount(entry: CatalogEntry): string {
  const messageCount = entry.userMessageCount + entry.assistantMessageCount;
  const messages = `${messageCount} message${messageCount === 1 ? "" : "s"}`;
  if (entry.toolResultCount === 0) return messages;
  return `${messages} · ${entry.toolResultCount} tool${entry.toolResultCount === 1 ? "" : "s"}`;
}
